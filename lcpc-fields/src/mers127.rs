// Copyright 2026 RealBrakedownPerf Authors
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

//! 127-bit Mersenne prime field [`Mers127`] ($p = 2^{127} - 1$, Plan 02 §2.2).
//!
//! Elements are stored in canonical residue form $a \in [0, 2^{127} - 1)$ across two
//! little-endian `u64` limbs (`self.0[0]` = bits $0..63$, `self.0[1]` = bits $64..126$).
//!
//! Because $2^{127} \equiv 1 \pmod p$, modular reduction of both 256-bit products and
//! the 320-bit 5-limb [`Mers127Acc`] accumulator (`[u64; 5]`) requires **zero**
//! multiplications—only shift, mask, and addition operations.

use ff::{Field, PrimeField};
use rand_core::TryRng;
use serde::{Deserialize, Serialize};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq, CtOption};

use crate::{impl_field_ops_and_num, LazyField};

/// Element of the 127-bit Mersenne prime field $\mathbb{F}_{2^{127}-1}$, stored as a
/// canonical residue $a \in [0, 2^{127} - 1)$ across two native little-endian `u64` limbs.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mers127(pub [u64; 2]);

/// 5-limb (320-bit) unreduced accumulator for [`Mers127`] lazy MACs (Plan 02 §2.2).
///
/// Represents $\sum_{i=0}^4 c_i \cdot 2^{64i}$. Each product of two canonical elements is
/// $< (2^{127}-1)^2 < 2^{254}$, so up to $D < 2^{66}$ products fit in 320 bits without
/// overflow before [`reduce_mers127`].
#[repr(C, align(8))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mers127Acc {
    /// Little-endian 64-bit limbs `c[0] + c[1]*2^64 + c[2]*2^128 + c[3]*2^192 + c[4]*2^256`.
    pub c: [u64; 5],
}

impl Mers127 {
    /// Prime modulus $p = 2^{127} - 1$ as little-endian `[u64; 2]`.
    pub const MODULUS: [u64; 2] = [0xffffffffffffffff, 0x7fffffffffffffff];

    /// Prime modulus $p = 2^{127} - 1$ as a `u128`.
    pub const MODULUS_U128: u128 = (1u128 << 127) - 1;

    /// $p - 2 = 2^{127} - 3$ in little-endian `[u64; 2]` for Fermat inversion.
    const MODULUS_MINUS_2: [u64; 2] = [0xfffffffffffffffd, 0x7fffffffffffffff];

    /// Low 64-bit limb (bits $0..63$).
    #[inline(always)]
    pub const fn lo(&self) -> u64 {
        self.0[0]
    }

    /// High 64-bit limb (bits $64..126$; bit 63 is always 0 for canonical elements).
    #[inline(always)]
    pub const fn hi(&self) -> u64 {
        self.0[1]
    }

    /// Construct from a `u128` value by reducing modulo $2^{127} - 1$.
    #[inline(always)]
    pub const fn from_u128_reduced(x: u128) -> Self {
        let lo = x & Self::MODULUS_U128;
        let hi = x >> 127;
        let mut s = lo + hi;
        if s >= Self::MODULUS_U128 {
            s -= Self::MODULUS_U128;
        }
        Mers127([s as u64, (s >> 64) as u64])
    }

    /// Convert canonical element to `u128`.
    #[inline(always)]
    pub const fn to_u128(&self) -> u128 {
        (self.0[0] as u128) | ((self.0[1] as u128) << 64)
    }

    /// Returns `true` iff `self` is in canonical range `[0, 2^127 - 1)`.
    #[inline(always)]
    pub const fn is_valid(&self) -> bool {
        (self.0[1] < Self::MODULUS[1])
            || (self.0[1] == Self::MODULUS[1] && self.0[0] < Self::MODULUS[0])
    }

    /// Modular addition modulo $2^{127} - 1$.
    #[inline(always)]
    pub const fn add_val(a: Self, b: Self) -> Self {
        let mut s = a.to_u128() + b.to_u128();
        if s >= Self::MODULUS_U128 {
            s -= Self::MODULUS_U128;
        }
        Mers127([s as u64, (s >> 64) as u64])
    }

    /// Modular subtraction modulo $2^{127} - 1$.
    #[inline(always)]
    pub const fn sub_val(a: Self, b: Self) -> Self {
        let a128 = a.to_u128();
        let b128 = b.to_u128();
        let s = if a128 >= b128 {
            a128 - b128
        } else {
            (a128 + Self::MODULUS_U128) - b128
        };
        Mers127([s as u64, (s >> 64) as u64])
    }

    /// Modular negation modulo $2^{127} - 1$.
    #[inline(always)]
    pub const fn neg_val(a: Self) -> Self {
        if a.0[0] == 0 && a.0[1] == 0 {
            a
        } else {
            Mers127([
                Self::MODULUS[0] ^ a.0[0],
                Self::MODULUS[1] ^ a.0[1],
            ])
        }
    }

    /// Eager multiplication $a \cdot b \bmod (2^{127} - 1)$.
    #[inline(always)]
    pub const fn mul_val(a: Self, b: Self) -> Self {
        let a0 = a.0[0] as u128;
        let a1 = a.0[1] as u128;
        let b0 = b.0[0] as u128;
        let b1 = b.0[1] as u128;

        let p00 = a0 * b0;
        let c0 = p00 as u64;
        let carry0 = p00 >> 64;

        let p01 = a0 * b1 + carry0;
        let c1 = p01 as u64;
        let c2 = p01 >> 64;

        let p10 = a1 * b0 + (c1 as u128);
        let c1 = p10 as u64;
        let carry1 = p10 >> 64;

        let p11 = a1 * b1 + c2 + carry1;
        let c2 = p11 as u64;
        let c3 = (p11 >> 64) as u64;

        // Product < (2^127 - 1)^2 < 2^254 splits into two 127-bit halves:
        let lo_127 = (((c1 & 0x7fffffffffffffff) as u128) << 64) | (c0 as u128);
        let hi_127 = ((c3 as u128) << 65) | ((c2 as u128) << 1) | ((c1 >> 63) as u128);

        Self::from_u128_reduced(lo_127 + hi_127)
    }

    /// Eager squaring $a^2 \bmod (2^{127} - 1)$.
    #[inline(always)]
    pub const fn square_val(a: Self) -> Self {
        let a0 = a.0[0] as u128;
        let a1 = a.0[1] as u128;

        let p01 = a0 * a1;
        let r1 = p01 as u64;
        let r2 = (p01 >> 64) as u64;

        let r3 = r2 >> 63;
        let r2 = (r2 << 1) | (r1 >> 63);
        let r1 = r1 << 1;

        let p00 = a0 * a0;
        let c0 = p00 as u64;
        let carry0 = p00 >> 64;

        let s1 = (r1 as u128) + carry0;
        let c1 = s1 as u64;
        let carry1 = s1 >> 64;

        let p11 = a1 * a1 + (r2 as u128) + carry1;
        let c2 = p11 as u64;
        let carry2 = p11 >> 64;

        let c3 = r3.wrapping_add(carry2 as u64);

        let lo_127 = (((c1 & 0x7fffffffffffffff) as u128) << 64) | (c0 as u128);
        let hi_127 = ((c3 as u128) << 65) | ((c2 as u128) << 1) | ((c1 >> 63) as u128);

        Self::from_u128_reduced(lo_127 + hi_127)
    }

    /// Construct from a `u64` integer (already canonical since $2^{64} < 2^{127} - 1$).
    #[inline(always)]
    pub const fn from_u64_val(val: u64) -> Self {
        Mers127([val, 0])
    }
}

/// Unreduced lazy multiply-accumulate for [`Mers127`]: `acc += w * x` (Plan 02 §2.2).
#[inline(always)]
pub fn mac_mers127(acc: &mut Mers127Acc, w: &Mers127, x: &Mers127) {
    let w0 = w.0[0] as u128;
    let w1 = w.0[1] as u128;
    let x0 = x.0[0] as u128;
    let x1 = x.0[1] as u128;

    let p00 = w0 * x0;
    let t0 = p00 as u64;
    let c0 = p00 >> 64;

    let p01 = w0 * x1 + c0;
    let t1 = p01 as u64;
    let t2 = p01 >> 64;

    let p10 = w1 * x0 + (t1 as u128);
    let t1 = p10 as u64;
    let c1 = p10 >> 64;

    let p11 = w1 * x1 + t2 + c1;
    let t2 = p11 as u64;
    let t3 = (p11 >> 64) as u64;

    let s0 = (acc.c[0] as u128) + (t0 as u128);
    acc.c[0] = s0 as u64;
    let carry0 = s0 >> 64;

    let s1 = (acc.c[1] as u128) + (t1 as u128) + carry0;
    acc.c[1] = s1 as u64;
    let carry1 = s1 >> 64;

    let s2 = (acc.c[2] as u128) + (t2 as u128) + carry1;
    acc.c[2] = s2 as u64;
    let carry2 = s2 >> 64;

    let s3 = (acc.c[3] as u128) + (t3 as u128) + carry2;
    acc.c[3] = s3 as u64;
    let carry3 = (s3 >> 64) as u64;

    acc.c[4] = acc.c[4].wrapping_add(carry3);
}

/// Unreduced 5-limb accumulator addition for [`Mers127Acc`]: `acc += other`.
#[inline(always)]
pub fn add_mers127_acc(acc: &mut Mers127Acc, other: &Mers127Acc) {
    let mut carry = 0u128;
    for i in 0..5 {
        let sum = (acc.c[i] as u128) + (other.c[i] as u128) + carry;
        acc.c[i] = sum as u64;
        carry = sum >> 64;
    }
}

/// Shift-and-add Mersenne reduction of a 320-bit [`Mers127Acc`] modulo $2^{127} - 1$
/// with **zero multiplications** (Plan 02 §2.2).
#[inline(always)]
pub fn reduce_mers127(acc: Mers127Acc) -> Mers127 {
    const P: u128 = Mers127::MODULUS_U128;
    let c0 = acc.c[0] as u128;
    let c1 = acc.c[1] as u128;
    let c2 = acc.c[2] as u128;
    let c3 = acc.c[3] as u128;
    let c4 = acc.c[4] as u128;

    // Split the 320-bit accumulator into three chunks modulo 2^127 - 1:
    // A0 = bits 0..126 (< 2^127)
    let a0 = ((c1 & 0x7fffffffffffffff) << 64) | c0;
    // A1 = bits 127..253 (< 2^127)
    let a1 = ((c3 & 0x3fffffffffffffff) << 65) | (c2 << 1) | (c1 >> 63);
    // A2 = bits 254..319 (< 2^66)
    let a2 = (c4 << 2) | (c3 >> 62);

    // Fold A0 + A1 first so the intermediate sum never overflows u128 even if all 320 bits are 1
    let s01 = a0 + a1;
    let s01_folded = (s01 & P) + (s01 >> 127);
    let s = s01_folded + a2;
    let mut res = (s & P) + (s >> 127);
    if res >= P {
        res -= P;
    }
    Mers127([res as u64, (res >> 64) as u64])
}

/// Non-inlined [`Mers127`] column MAC kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn mac_column_mers127(acc: &mut [Mers127Acc], w: &Mers127, src: &[Mers127]) {
    assert_eq!(acc.len(), src.len());
    for (a, x) in acc.iter_mut().zip(src.iter()) {
        mac_mers127(a, w, x);
    }
}

/// Non-inlined [`Mers127`] column reduction kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn reduce_column_mers127(dst: &mut [Mers127], acc: &mut [Mers127Acc]) {
    assert_eq!(dst.len(), acc.len());
    for (d, a) in dst.iter_mut().zip(acc.iter_mut()) {
        *d = reduce_mers127(core::mem::take(a));
    }
}

impl LazyField for Mers127 {
    type Acc = Mers127Acc;

    /// `Mers127Acc` (320 bits) supports up to $D < 2^{66}$ unreduced MACs before
    /// `reduce` without intermediate folding (Plan 02 §4.2).
    const MAX_ACCUMULATIONS: usize = usize::MAX;

    #[inline(always)]
    fn prepare_weight(w: &Self) -> Self {
        *w
    }

    #[inline(always)]
    fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
        mac_mers127(acc, w, x);
    }

    #[inline(always)]
    fn add(acc: &mut Self::Acc, other: &Self::Acc) {
        add_mers127_acc(acc, other);
    }

    #[inline(always)]
    fn reduce(acc: Self::Acc) -> Self {
        reduce_mers127(acc)
    }
}

impl ConstantTimeEq for Mers127 {
    #[inline(always)]
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0[0].ct_eq(&other.0[0]) & self.0[1].ct_eq(&other.0[1])
    }
}

impl ConditionallySelectable for Mers127 {
    #[inline(always)]
    fn conditional_select(a: &Self, b: &Self, choice: Choice) -> Self {
        Mers127([
            u64::conditional_select(&a.0[0], &b.0[0], choice),
            u64::conditional_select(&a.0[1], &b.0[1], choice),
        ])
    }
}

impl Ord for Mers127 {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match self.0[1].cmp(&other.0[1]) {
            core::cmp::Ordering::Equal => self.0[0].cmp(&other.0[0]),
            ord => ord,
        }
    }
}

impl PartialOrd for Mers127 {
    #[inline(always)]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<u64> for Mers127 {
    #[inline(always)]
    fn from(val: u64) -> Self {
        Self::from_u64_val(val)
    }
}

impl_field_ops_and_num!(Mers127);

impl Field for Mers127 {
    const ZERO: Self = Mers127([0, 0]);
    const ONE: Self = Mers127([1, 0]);

    #[inline]
    fn try_random<R: TryRng + ?Sized>(rng: &mut R) -> Result<Self, R::Error> {
        loop {
            let lo = rng.try_next_u64()?;
            let hi = rng.try_next_u64()? & 0x7fffffffffffffffu64;
            let candidate = Mers127([lo, hi]);
            if candidate.is_valid() {
                return Ok(candidate);
            }
        }
    }

    #[inline(always)]
    fn is_zero_vartime(&self) -> bool {
        self.0[0] == 0 && self.0[1] == 0
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
        let inv = self.pow_vartime(Self::MODULUS_MINUS_2);
        CtOption::new(inv, !self.is_zero())
    }

    #[inline]
    fn sqrt_ratio(num: &Self, div: &Self) -> (Choice, Self) {
        ff::helpers::sqrt_ratio_generic(num, div)
    }

    #[inline]
    fn sqrt(&self) -> CtOption<Self> {
        // Because p = 2^127 - 1 = 3 (mod 4), (p + 1) / 4 = 2^125.
        // Thus x^((p + 1) / 4) is computed by 125 squarings.
        let mut sqrt = *self;
        for _ in 0..125 {
            sqrt = sqrt.square();
        }
        CtOption::new(sqrt, (sqrt * sqrt).ct_eq(self))
    }
}

impl PrimeField for Mers127 {
    type Repr = [u8; 16];

    #[inline]
    fn from_repr(repr: Self::Repr) -> CtOption<Self> {
        let lo = u64::from_le_bytes([
            repr[0], repr[1], repr[2], repr[3], repr[4], repr[5], repr[6], repr[7],
        ]);
        let hi = u64::from_le_bytes([
            repr[8], repr[9], repr[10], repr[11], repr[12], repr[13], repr[14], repr[15],
        ]);

        let (_, borrow0) = lo.overflowing_sub(Self::MODULUS[0]);
        let (d1, borrow1) = hi.overflowing_sub(Self::MODULUS[1]);
        let (_, borrow2) = d1.overflowing_sub(borrow0 as u64);
        let is_some = Choice::from((borrow1 | borrow2) as u8);

        CtOption::new(Mers127([lo, hi]), is_some)
    }

    #[inline]
    fn from_repr_vartime(repr: Self::Repr) -> Option<Self> {
        let lo = u64::from_le_bytes([
            repr[0], repr[1], repr[2], repr[3], repr[4], repr[5], repr[6], repr[7],
        ]);
        let hi = u64::from_le_bytes([
            repr[8], repr[9], repr[10], repr[11], repr[12], repr[13], repr[14], repr[15],
        ]);
        let candidate = Mers127([lo, hi]);
        if candidate.is_valid() {
            Some(candidate)
        } else {
            None
        }
    }

    #[inline]
    fn to_repr(&self) -> Self::Repr {
        let lo = self.0[0].to_le_bytes();
        let hi = self.0[1].to_le_bytes();
        [
            lo[0], lo[1], lo[2], lo[3], lo[4], lo[5], lo[6], lo[7], hi[0], hi[1], hi[2], hi[3],
            hi[4], hi[5], hi[6], hi[7],
        ]
    }

    #[inline(always)]
    fn is_odd(&self) -> Choice {
        Choice::from((self.0[0] & 1) as u8)
    }

    const MODULUS: &'static str = "0x7fffffffffffffffffffffffffffffff";
    const NUM_BITS: u32 = 127;
    const CAPACITY: u32 = 126;
    const TWO_INV: Self = Mers127([0x0000000000000000, 0x4000000000000000]);
    const MULTIPLICATIVE_GENERATOR: Self = Mers127([0x000000000000002b, 0x0000000000000000]);
    const S: u32 = 1;
    const ROOT_OF_UNITY: Self = Mers127([0xfffffffffffffffe, 0x7fffffffffffffff]);
    const ROOT_OF_UNITY_INV: Self = Mers127([0xfffffffffffffffe, 0x7fffffffffffffff]);
    const DELTA: Self = Mers127([0x0000000000000739, 0x0000000000000000]);
}
