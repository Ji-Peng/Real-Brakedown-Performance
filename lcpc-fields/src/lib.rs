// Copyright 2026 RealBrakedownPerf Authors
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

#![deny(missing_docs)]

//! Portable 64-bit scalar finite fields and the [`LazyField`] trait for the LCPC
//! Brakedown polynomial commitment scheme fork.
//!
//! # Architecture & Overflow Bounds (Plan 02 §4.1–§4.2, Decision D7)
//!
//! All arithmetic is implemented in portable stable Rust using native `u64` and `u128`
//! integer operations (`no std::arch`, `no core::simd`, `no asm!`).
//!
//! | Field | Modulus / Structure | Element Layout | Accumulator (`LazyField::Acc`) | Max Term per Edge ($\Delta$) | Mathematical Limit ($D_{\max}$) | `MAX_ACCUMULATIONS` |
//! | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
//! | [`Mont127`] | $q = \text{0x6e754097ba20e0bf}\_\text{7f2bd90000000001}$ | `[u64; 2]` (16 B, Montgomery $R = 2^{128}$) | [`Mont127Acc`] (`[u64; 5]`, 320 bits) | $q^2 < 2^{254}$ | $D \le 2^{64}$ (3-word CIOS reduction $R = 2^{192}$ guarantees $(A + Mq)/2^{192} < 1.5q < 2q$) | `usize::MAX` |
//! | [`Mers127`] | $p = 2^{127} - 1$ | `[u64; 2]` (16 B, canonical residue) | [`Mers127Acc`] (`[u64; 5]`, 320 bits) | $p^2 < 2^{254}$ | $D < 2^{66}$ (320-bit accumulator capacity $2^{320} - 1$) | `usize::MAX` |
//! | [`Fp2M61`] | $\mathbb{F}_{p^2} = \mathbb{F}_p[i]/(i^2+1), \; p = 2^{61} - 1$ | `{ real: u64, img: u64 }` (16 B) | [`Fp2M61Acc`] (`{ re: u128, im: u128 }`, 32 B) | $2(2^{61}-2)^2 < 2^{123}$ | $D \le 32$ ($32 \times 2^{123} = 2^{128}$) | `32` |

pub mod fp2m61;
pub mod mers127;
pub mod mont127;

pub use fp2m61::{
    fold_column_fp2_m61, mac_column_fp2_m61, reduce_column_fp2_m61, Fp2M61, Fp2M61Acc,
};
pub use mers127::{mac_column_mers127, reduce_column_mers127, Mers127, Mers127Acc};
pub use mont127::{mac_column_mont127, reduce_column_mont127, Mont127, Mont127Acc};

/// Trait abstracting unreduced lazy accumulation across finite fields (Plan 02 §4.1, D7).
///
/// Decouples batched right-to-left (R2L) expander graph traversal from field-specific
/// modular reduction math:
///
/// - During graph setup, each sampled weight $w$ is transformed once via
///   [`LazyField::prepare_weight`].
/// - During batched R2L evaluation of right node $r$, unreduced products $w \cdot x$ are
///   accumulated into [`LazyField::Acc`] via [`LazyField::mac`].
/// - If a right node's in-degree exceeds [`LazyField::MAX_ACCUMULATIONS`], the encoder
///   invokes [`LazyField::fold`] after every `MAX_ACCUMULATIONS` incident edges to
///   restore accumulator headroom for another `MAX_ACCUMULATIONS - 1` MAC operations.
/// - Once all incident edges of right node $r$ have been accumulated, a single call to
///   [`LazyField::reduce`] normalizes the accumulator to the canonical field element in
///   `Self`'s standard representation.
pub trait LazyField:
    ff::Field
    + num_traits::Num
    + lcpc_2d::SizedField
    + lcpc_2d::FieldHash
    + Copy
    + Default
    + Send
    + Sync
    + 'static
{
    /// Unreduced accumulator for batched graph MACs.
    type Acc: Copy + Default + Send + Sync + 'static;

    /// Documented maximum number of [`LazyField::mac`] calls on one accumulator before
    /// [`LazyField::fold`] or [`LazyField::reduce`] is required (`usize::MAX` when no
    /// chunking is needed).
    const MAX_ACCUMULATIONS: usize;

    /// Graph-setup transform of an edge weight into the form [`LazyField::mac`] expects
    /// (e.g. $w \cdot 2^{192} \bmod q$ for [`Mont127`]; identity for [`Mers127`]).
    #[inline(always)]
    fn prepare_weight(w: &Self) -> Self {
        *w
    }

    /// Accumulates `acc += w * x` without modular reduction.
    ///
    /// Here `w` must have been preprocessed with [`LazyField::prepare_weight`].
    fn mac(acc: &mut Self::Acc, w: &Self, x: &Self);

    /// Accumulates `acc += other` (unreduced).
    fn add(acc: &mut Self::Acc, other: &Self::Acc);

    /// Full reduction of `acc` to the canonical element (in `Self`'s normal representation).
    fn reduce(acc: Self::Acc) -> Self;

    /// Partial fold that restores headroom for another `MAX_ACCUMULATIONS - 1`
    /// MACs; a no-op for fields that never need it.
    #[inline(always)]
    fn fold(_acc: &mut Self::Acc) {}
}

/// Generic non-inlined column MAC kernel for codegen inspection (`scripts/check_codegen.py`).
///
/// Accumulates `acc[i] += w * src[i]` across a slice of rows using [`LazyField::mac`].
#[inline(never)]
pub fn mac_column<F: LazyField>(acc: &mut [F::Acc], w: &F, src: &[F]) {
    let n = acc.len().min(src.len());
    let acc = &mut acc[..n];
    let src = &src[..n];
    for (a, s) in acc.iter_mut().zip(src.iter()) {
        F::mac(a, w, s);
    }
}

/// Generic non-inlined column reduction kernel for codegen inspection (`scripts/check_codegen.py`).
///
/// Reduces each accumulator in `acc` via [`LazyField::reduce`], writing the canonical
/// field element to `dst` and zeroing `acc`.
#[inline(never)]
pub fn reduce_column<F: LazyField>(dst: &mut [F], acc: &mut [F::Acc]) {
    let n = dst.len().min(acc.len());
    let dst = &mut dst[..n];
    let acc = &mut acc[..n];
    for (d, a) in dst.iter_mut().zip(acc.iter_mut()) {
        *d = F::reduce(core::mem::take(a));
    }
}

/// Generic non-inlined column partial-fold kernel for codegen inspection (`scripts/check_codegen.py`).
#[inline(never)]
pub fn fold_column<F: LazyField>(acc: &mut [F::Acc]) {
    for a in acc.iter_mut() {
        F::fold(a);
    }
}

/// Implement standard arithmetic operator traits, `Sum`, `Product`, and `num_traits::Num`
/// for a field type `$ident` given inherent `add_val`, `sub_val`, `mul_val`, `neg_val`,
/// and `from_u64_val` methods.
///
/// Also implements `for<'a, 'b> &'a $ident: Mul<&'b $ident, Output = $ident>`, which
/// satisfies `sprs::MulAcc`'s blanket implementation (used by upstream LCPC's encoder
/// and the fork's test-only references).
#[macro_export]
macro_rules! impl_field_ops_and_num {
    ($ident:ident) => {
        impl ::core::ops::Neg for $ident {
            type Output = Self;

            #[inline(always)]
            fn neg(self) -> Self {
                Self::neg_val(self)
            }
        }

        impl ::core::ops::Add for $ident {
            type Output = Self;

            #[inline(always)]
            fn add(self, rhs: Self) -> Self {
                Self::add_val(self, rhs)
            }
        }

        impl<'a> ::core::ops::Add<&'a $ident> for $ident {
            type Output = Self;

            #[inline(always)]
            fn add(self, rhs: &'a Self) -> Self {
                Self::add_val(self, *rhs)
            }
        }

        impl ::core::ops::AddAssign for $ident {
            #[inline(always)]
            fn add_assign(&mut self, rhs: Self) {
                *self = Self::add_val(*self, rhs);
            }
        }

        impl<'a> ::core::ops::AddAssign<&'a $ident> for $ident {
            #[inline(always)]
            fn add_assign(&mut self, rhs: &'a Self) {
                *self = Self::add_val(*self, *rhs);
            }
        }

        impl ::core::ops::Sub for $ident {
            type Output = Self;

            #[inline(always)]
            fn sub(self, rhs: Self) -> Self {
                Self::sub_val(self, rhs)
            }
        }

        impl<'a> ::core::ops::Sub<&'a $ident> for $ident {
            type Output = Self;

            #[inline(always)]
            fn sub(self, rhs: &'a Self) -> Self {
                Self::sub_val(self, *rhs)
            }
        }

        impl ::core::ops::SubAssign for $ident {
            #[inline(always)]
            fn sub_assign(&mut self, rhs: Self) {
                *self = Self::sub_val(*self, rhs);
            }
        }

        impl<'a> ::core::ops::SubAssign<&'a $ident> for $ident {
            #[inline(always)]
            fn sub_assign(&mut self, rhs: &'a Self) {
                *self = Self::sub_val(*self, *rhs);
            }
        }

        impl ::core::ops::Mul for $ident {
            type Output = Self;

            #[inline(always)]
            fn mul(self, rhs: Self) -> Self {
                Self::mul_val(self, rhs)
            }
        }

        impl<'a> ::core::ops::Mul<&'a $ident> for $ident {
            type Output = Self;

            #[inline(always)]
            fn mul(self, rhs: &'a Self) -> Self {
                Self::mul_val(self, *rhs)
            }
        }

        impl<'a, 'b> ::core::ops::Mul<&'b $ident> for &'a $ident {
            type Output = $ident;

            #[inline(always)]
            fn mul(self, rhs: &'b $ident) -> $ident {
                $ident::mul_val(*self, *rhs)
            }
        }

        impl ::core::ops::MulAssign for $ident {
            #[inline(always)]
            fn mul_assign(&mut self, rhs: Self) {
                *self = Self::mul_val(*self, rhs);
            }
        }

        impl<'a> ::core::ops::MulAssign<&'a $ident> for $ident {
            #[inline(always)]
            fn mul_assign(&mut self, rhs: &'a Self) {
                *self = Self::mul_val(*self, *rhs);
            }
        }

        impl<T: ::core::borrow::Borrow<$ident>> ::core::iter::Sum<T> for $ident {
            fn sum<I: Iterator<Item = T>>(iter: I) -> Self {
                iter.fold(<Self as ::ff::Field>::ZERO, |acc, item| {
                    acc + item.borrow()
                })
            }
        }

        impl<T: ::core::borrow::Borrow<$ident>> ::core::iter::Product<T> for $ident {
            fn product<I: Iterator<Item = T>>(iter: I) -> Self {
                iter.fold(<Self as ::ff::Field>::ONE, |acc, item| acc * item.borrow())
            }
        }

        impl ::num_traits::Num for $ident {
            type FromStrRadixErr = ::std::num::ParseIntError;

            fn from_str_radix(s: &str, r: u32) -> Result<Self, Self::FromStrRadixErr> {
                if s.is_empty() {
                    return Err(u32::from_str_radix(s, r).err().unwrap());
                }

                if s == "0" {
                    return Ok(<Self as ::ff::Field>::ZERO);
                }

                let mut res = <Self as ::ff::Field>::ZERO;
                let radix = Self::from_u64_val(r as u64);
                let mut first_digit = true;
                for c in s.chars() {
                    match c.to_digit(r) {
                        Some(c) => {
                            if first_digit {
                                if c == 0 {
                                    return Err(u32::from_str_radix("3", 2).err().unwrap());
                                }
                                first_digit = false;
                            }

                            res *= &radix;
                            res += Self::from_u64_val(c as u64);
                        }
                        None => {
                            return Err(u32::from_str_radix("3", 2).err().unwrap());
                        }
                    }
                }
                Ok(res)
            }
        }

        impl ::num_traits::Zero for $ident {
            #[inline(always)]
            fn zero() -> Self {
                <Self as ::ff::Field>::ZERO
            }

            #[inline(always)]
            fn is_zero(&self) -> bool {
                bool::from(<Self as ::ff::Field>::is_zero(self))
            }
        }

        impl ::num_traits::One for $ident {
            #[inline(always)]
            fn one() -> Self {
                <Self as ::ff::Field>::ONE
            }

            #[inline(always)]
            fn is_one(&self) -> bool {
                self == &<Self as ::ff::Field>::ONE
            }
        }

        #[allow(clippy::suspicious_arithmetic_impl)]
        impl ::core::ops::Div<$ident> for $ident {
            type Output = Self;

            #[inline]
            fn div(self, rhs: Self) -> Self {
                self * <Self as ::ff::Field>::invert(&rhs).unwrap()
            }
        }

        #[allow(clippy::suspicious_arithmetic_impl)]
        impl ::core::ops::Div<&$ident> for $ident {
            type Output = Self;

            #[inline]
            fn div(self, rhs: &Self) -> Self {
                self * <Self as ::ff::Field>::invert(rhs).unwrap()
            }
        }

        impl ::core::ops::Rem<$ident> for $ident {
            type Output = Self;

            #[inline]
            fn rem(self, rhs: Self) -> Self {
                if bool::from(<Self as ::ff::Field>::is_zero(&rhs)) {
                    panic!("divide by zero");
                }
                <Self as ::ff::Field>::ZERO
            }
        }

        impl ::core::ops::Rem<&$ident> for $ident {
            type Output = Self;

            #[inline]
            fn rem(self, rhs: &Self) -> Self {
                if bool::from(<Self as ::ff::Field>::is_zero(rhs)) {
                    panic!("divide by zero");
                }
                <Self as ::ff::Field>::ZERO
            }
        }

        impl ::num_traits::ops::mul_add::MulAdd for $ident {
            type Output = Self;

            #[inline(always)]
            fn mul_add(mut self, a: Self, b: Self) -> Self {
                self *= &a;
                self += &b;
                self
            }
        }

        impl ::num_traits::ops::mul_add::MulAdd<&$ident, &$ident> for $ident {
            type Output = Self;

            #[inline(always)]
            fn mul_add(mut self, a: &Self, b: &Self) -> Self {
                self *= a;
                self += b;
                self
            }
        }
    };
}
