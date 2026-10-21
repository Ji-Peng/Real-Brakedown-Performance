// Copyright 2026 RealBrakedownPerf Authors
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

//! 127-bit Montgomery prime field [`Mont127`] ($q = \text{0x6e754097ba20e0bf7f2bd90000000001}$).
//!
//! Bit-exact in memory layout (`[u64; 2]` Montgomery form with radix $R = 2^{128}$),
//! `serde` serialization, `PrimeField::to_repr`, and `Field::random` rejection sampling
//! with `lcpc_test_fields::ft127::Ft127` (`ff_derive`).
//!
//! For batched R2L lazy evaluation ([`LazyField`]), edge weights are pre-scaled to
//! $w \cdot 2^{192} \bmod q$ during graph setup ([`LazyField::prepare_weight`]),
//! unreduced 256-bit schoolbook products are accumulated into the 5-limb (320-bit)
//! [`Mont127Acc`] accumulator ([`LazyField::mac`]), and a single 3-word CIOS
//! Montgomery reduction ($R = 2^{192}$) per right node ([`LazyField::reduce`]) returns
//! the canonical Montgomery residue $\sum_e x_e w_e \cdot 2^{128} \bmod q$.

use ff::{Field, PrimeField};
use rand_core::TryRng;
use serde::{Deserialize, Serialize};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq, CtOption};

use crate::{impl_field_ops_and_num, LazyField};

/// Element of the 127-bit prime field $\mathbb{F}_q$ with
/// $q = \text{0x6e754097ba20e0bf7f2bd90000000001}$, stored in Montgomery form
/// $A = a \cdot 2^{128} \bmod q$ across two native little-endian `u64` limbs.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mont127(pub [u64; 2]);

/// 5-limb (320-bit) unreduced accumulator for [`Mont127`] lazy MACs.
///
/// Represents $\sum_{i=0}^4 c_i \cdot 2^{64i}$. Each product of two canonical
/// `< q < 2^127` elements is `< q^2 < 2^254`, so up to $D \le 2^{64}$ products can be
/// accumulated before [`reduce_mont127`] without overflow, and the 3-word Montgomery
/// quotient satisfies $(A + M \cdot q) / 2^{192} < 1.5 q < 2q$.
#[repr(C, align(8))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mont127Acc {
    /// Little-endian 64-bit limbs `c[0] + c[1]*2^64 + c[2]*2^128 + c[3]*2^192 + c[4]*2^256`.
    pub c: [u64; 5],
}

impl Mont127 {
    /// Prime modulus $q = \text{0x6e754097ba20e0bf7f2bd90000000001}$ as little-endian `[u64; 2]`.
    pub const MODULUS: [u64; 2] = [0x7f2bd90000000001, 0x6e754097ba20e0bf];

    /// Prime modulus $q$ as a `u128`.
    pub const MODULUS_U128: u128 = 0x6e754097ba20e0bf_7f2bd90000000001;

    /// 64-bit word-level negative Montgomery inverse $\mu_0 = -q_0^{-1} \bmod 2^{64}$.
    pub const MU0: u64 = 0x7f2bd8ffffffffff;

    /// $R \bmod q = 2^{128} \bmod q$ (the multiplicative identity `ONE` in Montgomery form).
    pub const R_MOD_Q: Self = Mont127([0x01a84dfffffffffe, 0x23157ed08bbe3e81]);

    /// $R^2 \bmod q = 2^{256} \bmod q$ (used to convert standard integers into Montgomery form).
    pub const R2_MOD_Q: Self = Mont127([0x816bd5407cf6dce5, 0x2c1637057de6fce8]);

    /// $R_{192} \bmod q = 2^{192} \bmod q$ (used by [`LazyField::prepare_weight`] so that
    /// multiplying a standard Montgomery weight $w \cdot 2^{128} \bmod q$ by `R192_MOD_Q`
    /// yields $w \cdot 2^{192} \bmod q$).
    pub const R192_MOD_Q: Self = Mont127([0x5c743c756ecdaf14, 0x329b34e9ea44513d]);

    /// $q - 2$ in little-endian `[u64; 2]` for constant/variable-time Fermat inversion.
    const MODULUS_MINUS_2: [u64; 2] = [0x7f2bd8ffffffffff, 0x6e754097ba20e0bf];

    /// $(t - 1) / 2$ where $q - 1 = 2^S \cdot t$ ($S = 40$), used by Tonelli-Shanks `sqrt`.
    const TM1D2: [u64; 2] = [0x4bdd10705fbf95ec, 0x0000000000373aa0];

    /// Low 64-bit limb of the Montgomery representation.
    #[inline(always)]
    pub const fn lo(&self) -> u64 {
        self.0[0]
    }

    /// High 64-bit limb of the Montgomery representation.
    #[inline(always)]
    pub const fn hi(&self) -> u64 {
        self.0[1]
    }

    /// Returns `true` iff `self` is in canonical range `[0, q)`.
    #[inline(always)]
    pub const fn is_valid(&self) -> bool {
        (self.0[1] < Self::MODULUS[1])
            || (self.0[1] == Self::MODULUS[1] && self.0[0] < Self::MODULUS[0])
    }

    /// Conditional subtraction of $q$ from $(lo, hi) < 2q$.
    #[inline(always)]
    const fn sub_modulus_if_ge(mut lo: u64, mut hi: u64) -> Self {
        let q0 = Self::MODULUS[0];
        let q1 = Self::MODULUS[1];
        if (hi > q1) || (hi == q1 && lo >= q0) {
            let (sub_lo, borrow) = lo.overflowing_sub(q0);
            lo = sub_lo;
            hi = hi.wrapping_sub(q1).wrapping_sub(borrow as u64);
        }
        Mont127([lo, hi])
    }

    /// 2-word CIOS Montgomery reduction of a 256-bit integer `(r0, r1, r2, r3) < q * 2^128`,
    /// computing $(r_0 + r_1 2^{64} + r_2 2^{128} + r_3 2^{192}) \cdot 2^{-128} \bmod q$.
    #[inline(always)]
    pub const fn mont_reduce_256(r0: u64, mut r1: u64, mut r2: u64, mut r3: u64) -> Self {
        let q0 = Self::MODULUS[0] as u128;
        let q1 = Self::MODULUS[1] as u128;

        // Iteration 0: clear r0
        let k0 = r0.wrapping_mul(Self::MU0) as u128;
        let carry0 = ((r0 as u128) + k0 * q0) >> 64;
        let s1 = (r1 as u128) + k0 * q1 + carry0;
        r1 = s1 as u64;
        let carry1 = s1 >> 64;
        let s2 = (r2 as u128) + carry1;
        r2 = s2 as u64;
        let carry2 = s2 >> 64;

        // Iteration 1: clear r1
        let k1 = r1.wrapping_mul(Self::MU0) as u128;
        let carry0_b = ((r1 as u128) + k1 * q0) >> 64;
        let s2_b = (r2 as u128) + k1 * q1 + carry0_b;
        r2 = s2_b as u64;
        let carry1_b = s2_b >> 64;
        let s3 = (r3 as u128) + carry2 + carry1_b;
        r3 = s3 as u64;

        Self::sub_modulus_if_ge(r2, r3)
    }

    /// Modular addition in Montgomery form.
    #[inline(always)]
    pub const fn add_val(a: Self, b: Self) -> Self {
        let (lo, carry) = a.0[0].overflowing_add(b.0[0]);
        let hi = a.0[1].wrapping_add(b.0[1]).wrapping_add(carry as u64);
        Self::sub_modulus_if_ge(lo, hi)
    }

    /// Modular subtraction in Montgomery form.
    #[inline(always)]
    pub const fn sub_val(a: Self, b: Self) -> Self {
        let (mut lo, borrow0) = a.0[0].overflowing_sub(b.0[0]);
        let (hi0, borrow1) = a.0[1].overflowing_sub(b.0[1]);
        let (mut hi, borrow2) = hi0.overflowing_sub(borrow0 as u64);
        if borrow1 || borrow2 {
            let (add_lo, carry) = lo.overflowing_add(Self::MODULUS[0]);
            lo = add_lo;
            hi = hi.wrapping_add(Self::MODULUS[1]).wrapping_add(carry as u64);
        }
        Mont127([lo, hi])
    }

    /// Modular negation in Montgomery form.
    #[inline(always)]
    pub const fn neg_val(a: Self) -> Self {
        if a.0[0] == 0 && a.0[1] == 0 {
            a
        } else {
            let (lo, borrow) = Self::MODULUS[0].overflowing_sub(a.0[0]);
            let hi = Self::MODULUS[1].wrapping_sub(a.0[1]).wrapping_sub(borrow as u64);
            Mont127([lo, hi])
        }
    }

    /// Eager Montgomery multiplication $a \cdot b \cdot 2^{-128} \bmod q$ (Plan 02 §2.1).
    #[inline(always)]
    pub const fn mul_val(a: Self, b: Self) -> Self {
        let a0 = a.0[0] as u128;
        let a1 = a.0[1] as u128;
        let b0 = b.0[0] as u128;
        let b1 = b.0[1] as u128;

        let p00 = a0 * b0;
        let t0 = p00 as u64;
        let c0 = p00 >> 64;

        let p01 = a0 * b1 + c0;
        let t1 = p01 as u64;
        let t2 = p01 >> 64;

        let p10 = a1 * b0 + (t1 as u128);
        let t1 = p10 as u64;
        let c1 = p10 >> 64;

        let p11 = a1 * b1 + t2 + c1;
        let t2 = p11 as u64;
        let t3 = (p11 >> 64) as u64;

        Self::mont_reduce_256(t0, t1, t2, t3)
    }

    /// Eager Montgomery squaring $a^2 \cdot 2^{-128} \bmod q$.
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
        let r0 = p00 as u64;
        let c0 = p00 >> 64;

        let s1 = (r1 as u128) + c0;
        let r1 = s1 as u64;
        let c1 = s1 >> 64;

        let p11 = a1 * a1 + (r2 as u128) + c1;
        let r2 = p11 as u64;
        let c2 = p11 >> 64;

        let r3 = r3.wrapping_add(c2 as u64);

        Self::mont_reduce_256(r0, r1, r2, r3)
    }

    /// Converts a `u64` integer into Montgomery form.
    #[inline(always)]
    pub const fn from_u64_val(val: u64) -> Self {
        Self::mul_val(Mont127([val, 0]), Self::R2_MOD_Q)
    }
}

/// Unreduced lazy multiply-accumulate for [`Mont127`]: `acc += w * x` (Plan 02 §2.1).
#[inline(always)]
pub fn mac_mont127(acc: &mut Mont127Acc, w: &Mont127, x: &Mont127) {
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

/// Unreduced 5-limb accumulator addition for [`Mont127Acc`]: `acc += other`.
#[inline(always)]
pub fn add_mont127_acc(acc: &mut Mont127Acc, other: &Mont127Acc) {
    let mut carry = 0u128;
    for i in 0..5 {
        let sum = (acc.c[i] as u128) + (other.c[i] as u128) + carry;
        acc.c[i] = sum as u64;
        carry = sum >> 64;
    }
}

/// 3-word CIOS Montgomery reduction of a 5-limb [`Mont127Acc`] with radix $R = 2^{192}$
/// (Plan 02 §2.1).
///
/// Divides `acc` by $2^{192} \bmod q$ in 3 word-aligned iterations (9 64-bit multiplications
/// and zero unaligned bit shifts), followed by a single conditional subtraction of $q$.
#[inline(always)]
pub fn reduce_mont127(mut acc: Mont127Acc) -> Mont127 {
    let q0 = Mont127::MODULUS[0] as u128;
    let q1 = Mont127::MODULUS[1] as u128;

    // 3 iterations of 64-bit word Montgomery reduction (divides by R = 2^192 mod q)
    for i in 0..3 {
        let u_i = acc.c[i].wrapping_mul(Mont127::MU0);
        let u = u_i as u128;

        let p0 = u * q0 + (acc.c[i] as u128); // bits 0..63 become exactly zero
        let carry0 = (p0 >> 64) as u64;

        let p1 = u * q1 + (acc.c[i + 1] as u128) + (carry0 as u128);
        acc.c[i + 1] = p1 as u64;
        let mut carry = (p1 >> 64) as u64;

        // Propagate carry across remaining upper limbs
        for j in (i + 2)..5 {
            let sum = (acc.c[j] as u128) + (carry as u128);
            acc.c[j] = sum as u64;
            carry = (sum >> 64) as u64;
        }
    }

    // Result resides in (acc.c[3], acc.c[4]) < 2q
    Mont127::sub_modulus_if_ge(acc.c[3], acc.c[4])
}

/// Non-inlined [`Mont127`] column MAC kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn mac_column_mont127(acc: &mut [Mont127Acc], w: &Mont127, src: &[Mont127]) {
    assert_eq!(acc.len(), src.len());
    for (a, x) in acc.iter_mut().zip(src.iter()) {
        mac_mont127(a, w, x);
    }
}

/// Non-inlined [`Mont127`] column reduction kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn reduce_column_mont127(dst: &mut [Mont127], acc: &mut [Mont127Acc]) {
    assert_eq!(dst.len(), acc.len());
    for (d, a) in dst.iter_mut().zip(acc.iter_mut()) {
        *d = reduce_mont127(core::mem::take(a));
    }
}

impl LazyField for Mont127 {
    type Acc = Mont127Acc;

    /// `Mont127Acc` (320 bits) supports up to $D \le 2^{64}$ unreduced MACs before
    /// `reduce` without intermediate folding (Plan 02 §4.2).
    const MAX_ACCUMULATIONS: usize = usize::MAX;

    #[inline(always)]
    fn prepare_weight(w: &Self) -> Self {
        // w is stored in standard Montgomery form w * 2^128 mod q.
        // Multiplying in Montgomery form by R192_MOD_Q = 2^192 mod q computes:
        // (w * 2^128) * 2^192 * 2^-128 = w * 2^192 mod q.
        Self::mul_val(*w, Self::R192_MOD_Q)
    }

    #[inline(always)]
    fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
        mac_mont127(acc, w, x);
    }

    #[inline(always)]
    fn add(acc: &mut Self::Acc, other: &Self::Acc) {
        add_mont127_acc(acc, other);
    }

    #[inline(always)]
    fn reduce(acc: Self::Acc) -> Self {
        reduce_mont127(acc)
    }
}

impl ConstantTimeEq for Mont127 {
    #[inline(always)]
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0[0].ct_eq(&other.0[0]) & self.0[1].ct_eq(&other.0[1])
    }
}

impl ConditionallySelectable for Mont127 {
    #[inline(always)]
    fn conditional_select(a: &Self, b: &Self, choice: Choice) -> Self {
        Mont127([
            u64::conditional_select(&a.0[0], &b.0[0], choice),
            u64::conditional_select(&a.0[1], &b.0[1], choice),
        ])
    }
}

impl Ord for Mont127 {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let a = Self::mont_reduce_256(self.0[0], self.0[1], 0, 0);
        let b = Self::mont_reduce_256(other.0[0], other.0[1], 0, 0);
        match a.0[1].cmp(&b.0[1]) {
            core::cmp::Ordering::Equal => a.0[0].cmp(&b.0[0]),
            ord => ord,
        }
    }
}

impl PartialOrd for Mont127 {
    #[inline(always)]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<u64> for Mont127 {
    #[inline(always)]
    fn from(val: u64) -> Self {
        Self::from_u64_val(val)
    }
}

impl_field_ops_and_num!(Mont127);

impl Field for Mont127 {
    const ZERO: Self = Mont127([0, 0]);
    const ONE: Self = Self::R_MOD_Q;

    /// Samples a uniform random element using the exact rejection loop of `ff_derive`
    /// (`Ft127`), ensuring bit-exact RNG stream consumption and Montgomery limbs (Plan 02 §4.4).
    #[inline]
    fn try_random<R: TryRng + ?Sized>(rng: &mut R) -> Result<Self, R::Error> {
        loop {
            let lo = rng.try_next_u64()?;
            let hi = rng.try_next_u64()? & 0x7fffffffffffffffu64;
            let candidate = Mont127([lo, hi]);
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
        ff::helpers::sqrt_tonelli_shanks(self, Self::TM1D2)
    }
}

impl PrimeField for Mont127 {
    type Repr = [u8; 16];

    #[inline]
    fn from_repr(repr: Self::Repr) -> CtOption<Self> {
        let lo = u64::from_le_bytes([
            repr[0], repr[1], repr[2], repr[3], repr[4], repr[5], repr[6], repr[7],
        ]);
        let hi = u64::from_le_bytes([
            repr[8], repr[9], repr[10], repr[11], repr[12], repr[13], repr[14], repr[15],
        ]);

        // Subtract modulus with borrow to check (lo, hi) < q in constant time
        let (_, borrow0) = lo.overflowing_sub(Self::MODULUS[0]);
        let (d1, borrow1) = hi.overflowing_sub(Self::MODULUS[1]);
        let (_, borrow2) = d1.overflowing_sub(borrow0 as u64);
        let is_some = Choice::from((borrow1 | borrow2) as u8);

        CtOption::new(Self::mul_val(Mont127([lo, hi]), Self::R2_MOD_Q), is_some)
    }

    #[inline]
    fn from_repr_vartime(repr: Self::Repr) -> Option<Self> {
        let lo = u64::from_le_bytes([
            repr[0], repr[1], repr[2], repr[3], repr[4], repr[5], repr[6], repr[7],
        ]);
        let hi = u64::from_le_bytes([
            repr[8], repr[9], repr[10], repr[11], repr[12], repr[13], repr[14], repr[15],
        ]);
        let raw = Mont127([lo, hi]);
        if raw.is_valid() {
            Some(Self::mul_val(raw, Self::R2_MOD_Q))
        } else {
            None
        }
    }

    #[inline]
    fn to_repr(&self) -> Self::Repr {
        let canonical = Self::mont_reduce_256(self.0[0], self.0[1], 0, 0);
        let lo = canonical.0[0].to_le_bytes();
        let hi = canonical.0[1].to_le_bytes();
        [
            lo[0], lo[1], lo[2], lo[3], lo[4], lo[5], lo[6], lo[7], hi[0], hi[1], hi[2], hi[3],
            hi[4], hi[5], hi[6], hi[7],
        ]
    }

    #[inline(always)]
    fn is_odd(&self) -> Choice {
        let canonical = Self::mont_reduce_256(self.0[0], self.0[1], 0, 0);
        Choice::from((canonical.0[0] & 1) as u8)
    }

    const MODULUS: &'static str = "0x6e754097ba20e0bf7f2bd90000000001";
    const NUM_BITS: u32 = 127;
    const CAPACITY: u32 = 126;
    const TWO_INV: Self = Mont127([0x80d426ffffffffff, 0x118abf6845df1f40]);
    const MULTIPLICATIVE_GENERATOR: Self = Mont127([0x04f8e9fffffffffa, 0x69407c71a33abb83]);
    const S: u32 = 40;
    const ROOT_OF_UNITY: Self = Mont127([0xf491a1dff39975f8, 0x178fd41c0f6a04fa]);
    const ROOT_OF_UNITY_INV: Self = Mont127([0xa9de5729f79c6899, 0x4eb95ab8120382b8]);
    const DELTA: Self = Mont127([0x75a2ae957737dae7, 0x477071b2ae78c0ae]);
}
