// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-test-fields, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.
//
// Modified 2026 by the RealBrakedownPerf authors (Apache-2.0, see NOTICE):
// current dependencies (BLAKE3 digest adapter); LazyField impls for test fields.
// The per-commit changes are in the git history since step/00-import
// (upstream conroi/lcpc f0e2116).
#![allow(clippy::too_many_arguments)]

use ff::Field;

/// Finite-field FFT support over `ff::PrimeField`
pub mod fft;

/// Portable-only BLAKE3 wrapper implementing `digest 0.10` traits.
///
/// `blake3 1.8.7` moved its optional `traits-preview` feature to `digest 0.11`,
/// whereas the workspace standardizes on `digest 0.10`. Wrapping `blake3::Hasher`
/// here provides a `digest::Digest` implementation backed by `blake3 1.8.7`
/// compiled with `pure + no_sse2 + no_sse41 + no_avx2 + no_avx512`.
#[derive(Clone, Default, Debug)]
pub struct Blake3(blake3::Hasher);

impl digest::HashMarker for Blake3 {}

impl digest::OutputSizeUser for Blake3 {
    type OutputSize = digest::consts::U32;
}

impl digest::Update for Blake3 {
    #[inline]
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}

impl digest::Reset for Blake3 {
    #[inline]
    fn reset(&mut self) {
        self.0.reset();
    }
}

impl digest::FixedOutput for Blake3 {
    #[inline]
    fn finalize_into(self, out: &mut digest::Output<Self>) {
        let h = self.0.finalize();
        out.copy_from_slice(h.as_bytes());
    }
}

impl digest::FixedOutputReset for Blake3 {
    #[inline]
    fn finalize_into_reset(&mut self, out: &mut digest::Output<Self>) {
        let h = self.0.finalize();
        out.copy_from_slice(h.as_bytes());
        self.0.reset();
    }
}

/// Implement `num_traits::Num` and associated traits for an `ff::PrimeField` type.
///
/// Replaces the unmaintained `ff-derive-num 0.2.0` proc-macro, which emitted
/// `<Self as ::ff::Field>::zero()` / `::one()` (removed in `ff 0.13+` in favor of
/// associated constants `Field::ZERO` and `Field::ONE`).
#[macro_export]
macro_rules! impl_num {
    ($ident:ident) => {
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
                let radix = Self::from(r as u64);
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
                            res += Self::from(c as u64);
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
            fn zero() -> Self {
                <Self as ::ff::Field>::ZERO
            }

            fn is_zero(&self) -> bool {
                bool::from(<Self as ::ff::Field>::is_zero(self))
            }
        }

        impl ::num_traits::One for $ident {
            fn one() -> Self {
                <Self as ::ff::Field>::ONE
            }

            fn is_one(&self) -> bool {
                self == &<Self as ::ff::Field>::ONE
            }
        }

        #[allow(clippy::suspicious_arithmetic_impl)]
        impl ::std::ops::Div<$ident> for $ident {
            type Output = Self;

            fn div(self, rhs: Self) -> Self {
                self * <Self as ::ff::Field>::invert(&rhs).unwrap()
            }
        }

        #[allow(clippy::suspicious_arithmetic_impl)]
        impl ::std::ops::Div<&$ident> for $ident {
            type Output = Self;

            fn div(self, rhs: &Self) -> Self {
                self * <Self as ::ff::Field>::invert(rhs).unwrap()
            }
        }

        impl ::std::ops::Rem<$ident> for $ident {
            type Output = Self;

            fn rem(self, _rhs: Self) -> Self {
                if bool::from(<Self as ::ff::Field>::is_zero(&self)) {
                    panic!("divide by zero");
                }

                <Self as ::ff::Field>::ZERO
            }
        }

        impl ::std::ops::Rem<&$ident> for $ident {
            type Output = Self;

            fn rem(self, _rhs: &Self) -> Self {
                if bool::from(<Self as ::ff::Field>::is_zero(&self)) {
                    panic!("divide by zero");
                }

                <Self as ::ff::Field>::ZERO
            }
        }

        impl<'a, 'b> ::std::ops::Mul<&'b $ident> for &'a $ident {
            type Output = $ident;

            #[inline]
            fn mul(self, rhs: &'b $ident) -> $ident {
                *self * rhs
            }
        }

        impl ::num_traits::ops::mul_add::MulAdd for $ident {
            type Output = Self;

            fn mul_add(mut self, a: Self, b: Self) -> Self {
                self *= &a;
                self += &b;
                self
            }
        }

        impl ::num_traits::ops::mul_add::MulAdd<&$ident, &$ident> for $ident {
            type Output = Self;

            fn mul_add(mut self, a: &Self, b: &Self) -> Self {
                self *= a;
                self += b;
                self
            }
        }
    };
}

pub mod ft63 {
    use ff::PrimeField;
    use serde::{Deserialize, Serialize};

    #[derive(PrimeField, Deserialize, Serialize)]
    #[PrimeFieldModulus = "5102708120182849537"]
    #[PrimeFieldGenerator = "10"]
    #[PrimeFieldReprEndianness = "little"]
    pub struct Ft63([u64; 1]);
    crate::impl_num!(Ft63);

    impl lcpc_fields::LazyField for Ft63 {
        type Acc = Self;
        const MAX_ACCUMULATIONS: usize = usize::MAX;

        #[inline(always)]
        fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
            *acc += *w * *x;
        }

        #[inline(always)]
        fn add(acc: &mut Self::Acc, other: &Self::Acc) {
            *acc += *other;
        }

        #[inline(always)]
        fn reduce(acc: Self::Acc) -> Self {
            acc
        }
    }
}

pub mod ft127 {
    use ff::PrimeField;
    use serde::{Deserialize, Serialize};

    #[derive(PrimeField, Deserialize, Serialize)]
    #[PrimeFieldModulus = "146823888364060453008360742206866194433"]
    #[PrimeFieldGenerator = "3"]
    #[PrimeFieldReprEndianness = "little"]
    pub struct Ft127([u64; 2]);
    crate::impl_num!(Ft127);

    impl lcpc_fields::LazyField for Ft127 {
        type Acc = lcpc_fields::Mont127Acc;
        const MAX_ACCUMULATIONS: usize = lcpc_fields::Mont127::MAX_ACCUMULATIONS;

        #[inline(always)]
        fn prepare_weight(w: &Self) -> Self {
            let m = unsafe { core::mem::transmute::<Self, lcpc_fields::Mont127>(*w) };
            let prep = <lcpc_fields::Mont127 as lcpc_fields::LazyField>::prepare_weight(&m);
            unsafe { core::mem::transmute::<lcpc_fields::Mont127, Self>(prep) }
        }

        #[inline(always)]
        fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
            let mw = unsafe { core::mem::transmute::<Self, lcpc_fields::Mont127>(*w) };
            let mx = unsafe { core::mem::transmute::<Self, lcpc_fields::Mont127>(*x) };
            <lcpc_fields::Mont127 as lcpc_fields::LazyField>::mac(acc, &mw, &mx);
        }

        #[inline(always)]
        fn add(acc: &mut Self::Acc, other: &Self::Acc) {
            <lcpc_fields::Mont127 as lcpc_fields::LazyField>::add(acc, other);
        }

        #[inline(always)]
        fn reduce(acc: Self::Acc) -> Self {
            let res = <lcpc_fields::Mont127 as lcpc_fields::LazyField>::reduce(acc);
            unsafe { core::mem::transmute::<lcpc_fields::Mont127, Self>(res) }
        }

        #[inline(always)]
        fn fold(acc: &mut Self::Acc) {
            <lcpc_fields::Mont127 as lcpc_fields::LazyField>::fold(acc);
        }
    }
}

pub mod ft191 {
    use ff::PrimeField;
    use serde::{Deserialize, Serialize};

    #[derive(PrimeField, Deserialize, Serialize)]
    #[PrimeFieldModulus = "1697146272512170708389931801544665676545308500647389167617"]
    #[PrimeFieldGenerator = "5"]
    #[PrimeFieldReprEndianness = "little"]
    pub struct Ft191([u64; 3]);
    crate::impl_num!(Ft191);

    impl lcpc_fields::LazyField for Ft191 {
        type Acc = Self;
        const MAX_ACCUMULATIONS: usize = usize::MAX;
        #[inline(always)]
        fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
            *acc += *w * *x;
        }
        #[inline(always)]
        fn add(acc: &mut Self::Acc, other: &Self::Acc) {
            *acc += *other;
        }
        #[inline(always)]
        fn reduce(acc: Self::Acc) -> Self {
            acc
        }
    }
}

pub mod ft255 {
    use ff::PrimeField;
    use serde::{Deserialize, Serialize};

    #[derive(PrimeField, Deserialize, Serialize)]
    #[PrimeFieldModulus = "46242760681095663677370860714659204618859642560429202607213929836750194081793"]
    #[PrimeFieldGenerator = "5"]
    #[PrimeFieldReprEndianness = "little"]
    pub struct Ft255([u64; 4]);
    crate::impl_num!(Ft255);

    impl lcpc_fields::LazyField for Ft255 {
        type Acc = Self;
        const MAX_ACCUMULATIONS: usize = usize::MAX;
        #[inline(always)]
        fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
            *acc += *w * *x;
        }
        #[inline(always)]
        fn add(acc: &mut Self::Acc, other: &Self::Acc) {
            *acc += *other;
        }
        #[inline(always)]
        fn reduce(acc: Self::Acc) -> Self {
            acc
        }
    }
}

/// generate random coeffs of length 2^`log_len`
pub fn random_coeffs<Ft: Field>(log_len: usize) -> Vec<Ft> {
    use std::io::{self, Write};
    use std::iter::repeat_with;

    let mut rng = rand::rng();
    let mut out = io::stderr();
    let spc = 1 << (if log_len > 6 { log_len - 6 } else { log_len });

    let ret = repeat_with(|| Ft::random(&mut rng))
        .enumerate()
        .take(1 << log_len)
        .inspect(|(c, _)| {
            if c % spc == 0 {
                out.write_all(b".").unwrap();
                out.flush().unwrap();
            }
        })
        .map(|(_, v)| v)
        .collect();
    out.write_all(b"\n").unwrap();
    out.flush().unwrap();
    ret
}
