// Copyright 2026 RealBrakedownPerf Authors
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

use ff::{Field, PrimeField};
use lcpc_2d::{FieldHash, SizedField};
use lcpc_fields::{
    fold_column, fold_column_fp2_m61, mac_column_fp2_m61, mac_column_mers127,
    mac_column_mont127, reduce_column_fp2_m61, reduce_column_mers127, reduce_column_mont127,
    Fp2M61, Fp2M61Acc, LazyField, Mers127, Mers127Acc, Mont127, Mont127Acc,
};
use lcpc_test_fields::ft127::Ft127;
use num_bigint::BigUint;
use num_traits::{Num, One, Zero};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

fn biguint_from_le_bytes(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_le(bytes)
}

fn biguint_to_repr16(v: &BigUint) -> [u8; 16] {
    let bytes = v.to_bytes_le();
    assert!(bytes.len() <= 16);
    let mut out = [0u8; 16];
    out[..bytes.len()].copy_from_slice(&bytes);
    out
}

fn biguint_from_acc5(c: &[u64; 5]) -> BigUint {
    let mut bytes = [0u8; 40];
    for (i, limb) in c.iter().enumerate() {
        bytes[i * 8..(i + 1) * 8].copy_from_slice(&limb.to_le_bytes());
    }
    BigUint::from_bytes_le(&bytes)
}

fn biguint_to_acc5(v: &BigUint) -> [u64; 5] {
    let bytes = v.to_bytes_le();
    assert!(bytes.len() <= 40);
    let mut padded = [0u8; 40];
    padded[..bytes.len()].copy_from_slice(&bytes);
    let mut c = [0u64; 5];
    for i in 0..5 {
        let mut limb = [0u8; 8];
        limb.copy_from_slice(&padded[i * 8..(i + 1) * 8]);
        c[i] = u64::from_le_bytes(limb);
    }
    c
}

#[test]
fn mont127_constants_match_bigint() {
    let q = BigUint::from(Mont127::MODULUS_U128);
    assert_eq!(
        q,
        BigUint::parse_bytes(b"146823888364060453008360742206866194433", 10).unwrap()
    );
    assert_eq!(q.bits(), 127);
    assert_eq!(<Mont127 as SizedField>::CLOG2, 127);
    assert_eq!(<Mont127 as SizedField>::FLOG2, 126);

    // Check MU0: q0 * MU0 = -1 (mod 2^64)
    let prod = Mont127::MODULUS[0].wrapping_mul(Mont127::MU0);
    assert_eq!(prod, u64::MAX);

    let r128 = (BigUint::one() << 128usize) % &q;
    let r192 = (BigUint::one() << 192usize) % &q;
    let r256 = (BigUint::one() << 256usize) % &q;

    let limbs_to_big = |limbs: [u64; 2]| -> BigUint {
        let val = (limbs[0] as u128) | ((limbs[1] as u128) << 64);
        BigUint::from(val)
    };
    assert_eq!(limbs_to_big(Mont127::R_MOD_Q.0), r128);
    assert_eq!(limbs_to_big(Mont127::R192_MOD_Q.0), r192);
    assert_eq!(limbs_to_big(Mont127::R2_MOD_Q.0), r256);

    // Check ONE and prepare_weight(ONE)
    assert_eq!(Mont127::ONE.to_repr(), biguint_to_repr16(&BigUint::one()));
    let prep_one = Mont127::prepare_weight(&Mont127::ONE);
    assert_eq!(prep_one, Mont127::R192_MOD_Q);

    // Check TWO_INV, MULTIPLICATIVE_GENERATOR, ROOT_OF_UNITY, ROOT_OF_UNITY_INV, DELTA
    let two = Mont127::from(2u64);
    assert_eq!(two * Mont127::TWO_INV, Mont127::ONE);
    assert_eq!(
        Mont127::MULTIPLICATIVE_GENERATOR,
        Mont127::from(3u64)
    );
    assert_eq!(
        Mont127::ROOT_OF_UNITY * Mont127::ROOT_OF_UNITY_INV,
        Mont127::ONE
    );
    // ROOT_OF_UNITY^(2^S) == 1 and ROOT_OF_UNITY^(2^(S-1)) != 1
    let mut r = Mont127::ROOT_OF_UNITY;
    for _ in 0..(Mont127::S - 1) {
        r = r.square();
    }
    assert_ne!(r, Mont127::ONE);
    assert_eq!(r.square(), Mont127::ONE);
}

#[test]
fn mers127_constants_match_bigint() {
    let p = BigUint::from(Mers127::MODULUS_U128);
    assert_eq!(p, (BigUint::one() << 127usize) - BigUint::one());
    assert_eq!(p.bits(), 127);
    assert_eq!(<Mers127 as SizedField>::CLOG2, 127);
    assert_eq!(<Mers127 as SizedField>::FLOG2, 126);

    let two = Mers127::from(2u64);
    assert_eq!(two * Mers127::TWO_INV, Mers127::ONE);
    assert_eq!(
        Mers127::MULTIPLICATIVE_GENERATOR,
        Mers127::from(43u64)
    );
    assert_eq!(Mers127::S, 1);
    assert_eq!(
        Mers127::ROOT_OF_UNITY * Mers127::ROOT_OF_UNITY_INV,
        Mers127::ONE
    );
    assert_ne!(Mers127::ROOT_OF_UNITY, Mers127::ONE);
    assert_eq!(Mers127::ROOT_OF_UNITY.square(), Mers127::ONE);
    assert_eq!(
        Mers127::MULTIPLICATIVE_GENERATOR.square(),
        Mers127::DELTA
    );
}

#[test]
fn mont127_differential_vs_ft127() {
    let mut rng_a = ChaCha20Rng::seed_from_u64(0xdeadbeef);
    let mut rng_b = ChaCha20Rng::seed_from_u64(0xdeadbeef);

    for i in 0..10_000 {
        let fa = Ft127::random(&mut rng_a);
        let fb = Mont127::random(&mut rng_b);
        assert_eq!(
            fa.to_repr().as_ref(),
            fb.to_repr().as_ref(),
            "to_repr mismatch at iteration {i}"
        );
        assert_eq!(
            fa.to_hash_repr().as_ref(),
            fb.to_hash_repr().as_ref(),
            "to_hash_repr mismatch at {i}"
        );
        // Compare internal Montgomery form through bincode and serde_json
        assert_eq!(
            bincode::serialize(&fa).unwrap(),
            bincode::serialize(&fb).unwrap(),
            "bincode internal representation mismatch at iteration {i}"
        );
        if i < 200 {
            assert_eq!(
                serde_json::to_string(&fa).unwrap(),
                serde_json::to_string(&fb).unwrap()
            );
        }
    }

    // Differential arithmetic checks across 2,000 random pairs
    for _ in 0..2_000 {
        let a_ft = Ft127::random(&mut rng_a);
        let b_ft = Ft127::random(&mut rng_a);
        let a_m = Mont127::random(&mut rng_b);
        let b_m = Mont127::random(&mut rng_b);

        assert_eq!((a_ft + b_ft).to_repr().as_ref(), (a_m + b_m).to_repr().as_ref());
        assert_eq!((a_ft - b_ft).to_repr().as_ref(), (a_m - b_m).to_repr().as_ref());
        assert_eq!((-a_ft).to_repr().as_ref(), (-a_m).to_repr().as_ref());
        assert_eq!((a_ft * b_ft).to_repr().as_ref(), (a_m * b_m).to_repr().as_ref());
        assert_eq!(a_ft.square().to_repr().as_ref(), a_m.square().to_repr().as_ref());
        assert_eq!(a_ft.double().to_repr().as_ref(), a_m.double().to_repr().as_ref());
        assert_eq!(bool::from(a_ft.is_odd()), bool::from(a_m.is_odd()));
    }

    // Differential inversion & square-root checks
    for _ in 0..200 {
        let a_ft = Ft127::random(&mut rng_a);
        let b_ft = Ft127::random(&mut rng_a);
        let a_m = Mont127::random(&mut rng_b);
        let b_m = Mont127::random(&mut rng_b);

        assert_eq!(
            a_ft.invert().unwrap().to_repr().as_ref(),
            a_m.invert().unwrap().to_repr().as_ref()
        );
        assert_eq!(
            (a_ft / b_ft).to_repr().as_ref(),
            (a_m / b_m).to_repr().as_ref()
        );

        let sq_ft = a_ft.square().sqrt().unwrap();
        let sq_m = a_m.square().sqrt().unwrap();
        assert_eq!(sq_ft.to_repr().as_ref(), sq_m.to_repr().as_ref());

        let (c_ft, r_ft) = Ft127::sqrt_ratio(&a_ft, &b_ft);
        let (c_m, r_m) = Mont127::sqrt_ratio(&a_m, &b_m);
        assert_eq!(bool::from(c_ft), bool::from(c_m));
        assert_eq!(r_ft.to_repr().as_ref(), r_m.to_repr().as_ref());
    }
}

fn check_field_axioms<F: LazyField + PrimeField<Repr = [u8; 16]>>(seed: u64)
where
    <F as Num>::FromStrRadixErr: core::fmt::Debug,
{
    let mut rng = ChaCha20Rng::seed_from_u64(seed);

    assert_eq!(F::ZERO + F::ZERO, F::ZERO);
    assert_eq!(F::ZERO - F::ZERO, F::ZERO);
    assert_eq!(-F::ZERO, F::ZERO);
    assert_eq!(F::ZERO * F::ONE, F::ZERO);
    assert_eq!(F::ONE * F::ONE, F::ONE);
    assert!(bool::from(ff::Field::is_zero(&F::ZERO)));
    assert!(!bool::from(ff::Field::is_zero(&F::ONE)));
    assert!(bool::from(F::ZERO.invert().is_none()));
    assert_eq!(F::ONE.invert().unwrap(), F::ONE);

    for _ in 0..1_000 {
        let x = F::random(&mut rng);
        let y = F::random(&mut rng);
        let z = F::random(&mut rng);

        // Additive axioms
        assert_eq!(x + F::ZERO, x);
        assert_eq!(F::ZERO + x, x);
        assert_eq!(x + (-x), F::ZERO);
        assert_eq!(x - x, F::ZERO);
        assert_eq!(x + y, y + x);
        assert_eq!((x + y) + z, x + (y + z));
        assert_eq!(x.double(), x + x);

        // Multiplicative axioms
        assert_eq!(x * F::ONE, x);
        assert_eq!(F::ONE * x, x);
        assert_eq!(x * F::ZERO, F::ZERO);
        assert_eq!(x * y, y * x);
        assert_eq!((x * y) * z, x * (y * z));
        assert_eq!(x.square(), x * x);
        assert_eq!(x.cube(), x * x * x);

        // Distributivity
        assert_eq!(x * (y + z), x * y + x * z);
        assert_eq!((x - y) * z, x * z - y * z);

        // Serialization round-trip
        let repr = x.to_repr();
        let recovered = F::from_repr(repr).unwrap();
        assert_eq!(x, recovered);
        assert_eq!(F::from_repr_vartime(repr), Some(x));
    }

    for _ in 0..200 {
        let x = F::random(&mut rng);
        if !bool::from(ff::Field::is_zero(&x)) {
            let inv = x.invert().unwrap();
            assert_eq!(x * inv, F::ONE);
        }
        let sq = x.square();
        let root = sq.sqrt().unwrap();
        assert_eq!(root.square(), sq);
    }

    // Check Num::from_str_radix and PrimeField::from_str_vartime
    assert_eq!(<F as Num>::from_str_radix("0", 10).unwrap(), F::ZERO);
    assert_eq!(<F as Num>::from_str_radix("1", 10).unwrap(), F::ONE);
    assert_eq!(
        <F as Num>::from_str_radix("deadbeef12345678", 16).unwrap(),
        F::from(0xdeadbeef12345678u64)
    );
    assert_eq!(
        F::from_str_vartime("12345678901234567890").unwrap(),
        F::from(12345678901234567890u64)
    );
}

#[test]
fn mont127_and_mers127_axioms() {
    check_field_axioms::<Mont127>(0x1270_0001);
    check_field_axioms::<Mers127>(0x1270_0002);
}

fn check_differential_vs_bigint<F>(modulus: &BigUint, seed: u64)
where
    F: LazyField + PrimeField<Repr = [u8; 16]>,
{
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let q = modulus;

    // Edge-case elements: 0, 1, 2, q-2, q-1, 2^64-1, 2^64, 2^64+1, 2^126-1, 2^126
    let edge_bigs: Vec<BigUint> = vec![
        BigUint::zero(),
        BigUint::one(),
        BigUint::from(2u64),
        q - BigUint::from(2u64),
        q - BigUint::one(),
        BigUint::from(u64::MAX),
        BigUint::one() << 64usize,
        (BigUint::one() << 64usize) + BigUint::one(),
        (BigUint::one() << 126usize) - BigUint::one(),
        BigUint::one() << 126usize,
    ];

    let edge_elems: Vec<F> = edge_bigs
        .iter()
        .map(|b| F::from_repr(biguint_to_repr16(b)).unwrap())
        .collect();

    // All pairs of edge cases
    for (i, a_big) in edge_bigs.iter().enumerate() {
        let a = edge_elems[i];
        assert_eq!(&biguint_from_le_bytes(&a.to_repr()), a_big);

        let neg_expected = if a_big.is_zero() {
            BigUint::zero()
        } else {
            q - a_big
        };
        assert_eq!(biguint_from_le_bytes(&(-a).to_repr()), neg_expected);

        for (j, b_big) in edge_bigs.iter().enumerate() {
            let b = edge_elems[j];

            let add_exp = (a_big + b_big) % q;
            let sub_exp = (a_big + q - b_big) % q;
            let mul_exp = (a_big * b_big) % q;

            assert_eq!(biguint_from_le_bytes(&(a + b).to_repr()), add_exp);
            assert_eq!(biguint_from_le_bytes(&(a - b).to_repr()), sub_exp);
            assert_eq!(biguint_from_le_bytes(&(a * b).to_repr()), mul_exp);

            // Single-edge LazyField::mac + reduce
            let mut acc = F::Acc::default();
            let w_prep = F::prepare_weight(&a);
            F::mac(&mut acc, &w_prep, &b);
            let reduced = F::reduce(acc);
            assert_eq!(biguint_from_le_bytes(&reduced.to_repr()), mul_exp);
        }
    }

    // Non-canonical from_repr rejection tests
    for invalid in [
        q.clone(),
        q + BigUint::one(),
        (BigUint::one() << 127usize) - BigUint::one(),
        BigUint::one() << 127usize,
        (BigUint::one() << 128usize) - BigUint::one(),
    ] {
        if &invalid >= q {
            let repr = biguint_to_repr16(&invalid);
            assert!(bool::from(F::from_repr(repr).is_none()));
            assert!(F::from_repr_vartime(repr).is_none());
        }
    }

    // Random differential tests vs BigUint
    for _ in 0..3_000 {
        let a = F::random(&mut rng);
        let b = F::random(&mut rng);
        let a_big = biguint_from_le_bytes(&a.to_repr());
        let b_big = biguint_from_le_bytes(&b.to_repr());

        assert_eq!(
            biguint_from_le_bytes(&(a + b).to_repr()),
            (&a_big + &b_big) % q
        );
        assert_eq!(
            biguint_from_le_bytes(&(a - b).to_repr()),
            (&a_big + q - &b_big) % q
        );
        assert_eq!(
            biguint_from_le_bytes(&(a * b).to_repr()),
            (&a_big * &b_big) % q
        );
        assert_eq!(
            biguint_from_le_bytes(&a.square().to_repr()),
            (&a_big * &a_big) % q
        );
    }

    // Multi-edge lazy MAC + fold + add + reduce vs BigUint across degrees 1..=200
    for deg in [1usize, 2, 7, 16, 31, 32, 33, 64, 128, 200] {
        let mut acc = F::Acc::default();
        let mut sum_big = BigUint::zero();
        for e in 0..deg {
            let w = F::random(&mut rng);
            let x = F::random(&mut rng);
            let w_big = biguint_from_le_bytes(&w.to_repr());
            let x_big = biguint_from_le_bytes(&x.to_repr());
            sum_big = (sum_big + w_big * x_big) % q;

            let w_prep = F::prepare_weight(&w);
            F::mac(&mut acc, &w_prep, &x);
            if (e + 1) % 32 == 0 {
                F::fold(&mut acc);
            }
        }
        let got = F::reduce(acc);
        assert_eq!(
            biguint_from_le_bytes(&got.to_repr()),
            sum_big,
            "lazy MAC mismatch at degree {deg}"
        );
    }
}

#[test]
fn mont127_and_mers127_differential_vs_bigint() {
    let q_mont = BigUint::from(Mont127::MODULUS_U128);
    let p_mers = BigUint::from(Mers127::MODULUS_U128);

    check_differential_vs_bigint::<Mont127>(&q_mont, 0xaaaa_1111);
    check_differential_vs_bigint::<Mers127>(&p_mers, 0xbbbb_2222);
}

#[test]
fn mont127_max_accumulator_fill_and_boundaries() {
    let q = BigUint::from(Mont127::MODULUS_U128);
    let q_minus_1_repr = biguint_to_repr16(&(&q - BigUint::one()));
    let q_minus_1 = Mont127::from_repr(q_minus_1_repr).unwrap();

    // 1. Accumulate 512 maximal products (q - 1) * (q - 1) via LazyField::mac
    let w_prep = Mont127::prepare_weight(&q_minus_1);
    let mut acc = Mont127Acc::default();
    let count = 512usize;
    for _ in 0..count {
        Mont127::mac(&mut acc, &w_prep, &q_minus_1);
    }
    let reduced = Mont127::reduce(acc);
    // Expected: count * (-1) * (-1) = count (mod q)
    assert_eq!(reduced, Mont127::from(count as u64));

    // 2. Test LazyField::add combining two accumulators
    let mut acc_half = Mont127Acc::default();
    for _ in 0..(count / 2) {
        Mont127::mac(&mut acc_half, &w_prep, &q_minus_1);
    }
    let mut acc_combined = acc_half;
    Mont127::add(&mut acc_combined, &acc_half);
    assert_eq!(Mont127::reduce(acc_combined), reduced);

    // 3. Direct mathematical limit test:
    // Construct accumulator corresponding to D = 2^64 maximal products (q - 1)^2,
    // and also A = 2^192 * q - 1 (the absolute ceiling before 2q overflow).
    let r192 = BigUint::one() << 192usize;
    let r128 = BigUint::one() << 128usize;
    // In Montgomery form (R = 2^128), reduce_mont127(A) computes A * 2^-192 mod q,
    // which represents the field element (A * 2^-192 * 2^-128) mod q = A * 2^-320 mod q.
    // Let's verify against raw Montgomery limbs: reduce_mont127(A).0 == (A * 2^-192) mod q.
    let r192_inv = {
        // 2^(192 * (q - 2)) mod q
        let q_minus_2 = &q - BigUint::from(2u64);
        r192.modpow(&q_minus_2, &q)
    };
    let r128_inv = {
        let q_minus_2 = &q - BigUint::from(2u64);
        r128.modpow(&q_minus_2, &q)
    };

    let qm1 = &q - BigUint::one();
    let max_d_64 = (BigUint::one() << 64usize) * &qm1 * &qm1;
    let max_ceiling = (&r192 * &q) - BigUint::one();

    for a_big in [max_d_64, max_ceiling] {
        let acc = Mont127Acc {
            c: biguint_to_acc5(&a_big),
        };
        assert_eq!(biguint_from_acc5(&acc.c), a_big);

        let res = Mont127::reduce(acc);
        // Raw Montgomery integer is (a_big * 2^-192) mod q:
        let expected_mont_limbs = (&a_big * &r192_inv) % &q;
        let got_mont_limbs =
            BigUint::from((res.0[0] as u128) | ((res.0[1] as u128) << 64));
        assert_eq!(got_mont_limbs, expected_mont_limbs);

        // And canonical integer via to_repr() is (expected_mont_limbs * 2^-128) mod q:
        let expected_canonical = (&expected_mont_limbs * &r128_inv) % &q;
        assert_eq!(biguint_from_le_bytes(&res.to_repr()), expected_canonical);
    }
}

#[test]
fn mers127_max_accumulator_fill_and_boundaries() {
    let p = BigUint::from(Mers127::MODULUS_U128);
    let p_minus_1 = Mers127([u64::MAX - 1, 0x7fffffffffffffff]);
    assert_eq!(
        biguint_from_le_bytes(&p_minus_1.to_repr()),
        &p - BigUint::one()
    );

    // 1. Accumulate 512 maximal products (p - 1) * (p - 1) via LazyField::mac
    let w_prep = Mers127::prepare_weight(&p_minus_1);
    let mut acc = Mers127Acc::default();
    let count = 512usize;
    for _ in 0..count {
        Mers127::mac(&mut acc, &w_prep, &p_minus_1);
    }
    let reduced = Mers127::reduce(acc);
    assert_eq!(reduced, Mers127::from(count as u64));

    // 2. Direct mathematical limit test:
    // Test D = 2^66 maximal products (p - 1)^2, and all-ones 320-bit accumulator [u64::MAX; 5]
    let pm1 = &p - BigUint::one();
    let max_d_66 = (BigUint::one() << 66usize) * &pm1 * &pm1;
    let all_ones_320 = (BigUint::one() << 320usize) - BigUint::one();
    // Also test exact chunk boundary values where A0 = p, A1 = p, A2 = 0..4
    let boundary_p_p = &p + (&p << 127usize);
    let boundary_p_p_1 = &boundary_p_p + (BigUint::one() << 254usize);

    for a_big in [max_d_66, all_ones_320, boundary_p_p, boundary_p_p_1] {
        let acc = Mers127Acc {
            c: biguint_to_acc5(&a_big),
        };
        let res = Mers127::reduce(acc);
        let expected = &a_big % &p;
        assert_eq!(biguint_from_le_bytes(&res.to_repr()), expected);
        assert!(res.is_valid());
    }
}

#[test]
fn non_inlined_column_kernels_match_scalar_oracle() {
    let mut rng = ChaCha20Rng::seed_from_u64(0x9999_8888);
    let rows = 64usize;

    // Mont127 column kernel check
    let w_m = Mont127::random(&mut rng);
    let w_m_prep = Mont127::prepare_weight(&w_m);
    let src_m: Vec<Mont127> = (0..rows).map(|_| Mont127::random(&mut rng)).collect();
    let mut acc_m = vec![Mont127Acc::default(); rows];
    let mut dst_m = vec![Mont127::ZERO; rows];

    mac_column_mont127(&mut acc_m, &w_m_prep, &src_m);
    fold_column::<Mont127>(&mut acc_m);
    mac_column_mont127(&mut acc_m, &w_m_prep, &src_m);
    reduce_column_mont127(&mut dst_m, &mut acc_m);

    for i in 0..rows {
        let expected = (w_m * src_m[i]).double();
        assert_eq!(dst_m[i], expected);
        assert_eq!(acc_m[i], Mont127Acc::default());
    }

    // Mers127 column kernel check
    let w_p = Mers127::random(&mut rng);
    let w_p_prep = Mers127::prepare_weight(&w_p);
    let src_p: Vec<Mers127> = (0..rows).map(|_| Mers127::random(&mut rng)).collect();
    let mut acc_p = vec![Mers127Acc::default(); rows];
    let mut dst_p = vec![Mers127::ZERO; rows];

    mac_column_mers127(&mut acc_p, &w_p_prep, &src_p);
    fold_column::<Mers127>(&mut acc_p);
    mac_column_mers127(&mut acc_p, &w_p_prep, &src_p);
    reduce_column_mers127(&mut dst_p, &mut acc_p);

    for i in 0..rows {
        let expected = (w_p * src_p[i]).double();
        assert_eq!(dst_p[i], expected);
        assert_eq!(acc_p[i], Mers127Acc::default());
    }
}

// ============================================================================
// Fp2M61 TESTS
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
struct BigUintFp2 {
    real: BigUint,
    img: BigUint,
}

impl BigUintFp2 {
    fn new(real: BigUint, img: BigUint) -> Self {
        let p = BigUint::from(Fp2M61::MODULUS_P);
        Self {
            real: real % &p,
            img: img % &p,
        }
    }

    fn from_elem(x: &Fp2M61) -> Self {
        Self {
            real: BigUint::from(x.real),
            img: BigUint::from(x.img),
        }
    }

    fn add(&self, other: &Self) -> Self {
        let p = BigUint::from(Fp2M61::MODULUS_P);
        Self {
            real: (&self.real + &other.real) % &p,
            img: (&self.img + &other.img) % &p,
        }
    }

    fn sub(&self, other: &Self) -> Self {
        let p = BigUint::from(Fp2M61::MODULUS_P);
        Self {
            real: (&self.real + &p - (&other.real % &p)) % &p,
            img: (&self.img + &p - (&other.img % &p)) % &p,
        }
    }

    fn neg(&self) -> Self {
        let p = BigUint::from(Fp2M61::MODULUS_P);
        Self {
            real: if self.real.is_zero() {
                BigUint::zero()
            } else {
                &p - &self.real
            },
            img: if self.img.is_zero() {
                BigUint::zero()
            } else {
                &p - &self.img
            },
        }
    }

    fn mul(&self, other: &Self) -> Self {
        let p = BigUint::from(Fp2M61::MODULUS_P);
        let p2 = &p * &p;
        // Re = real * other.real - img * other.img mod p
        let re_pos = &self.real * &other.real;
        let re_neg = &self.img * &other.img;
        let real = (&re_pos + &p2 - (&re_neg % &p)) % &p;

        // Im = real * other.img + img * other.real mod p
        let img = (&self.real * &other.img + &self.img * &other.real) % &p;
        Self { real, img }
    }

    fn square(&self) -> Self {
        self.mul(self)
    }

    fn double(&self) -> Self {
        self.add(self)
    }
}

fn assert_fp2_eq_biguint(got: &Fp2M61, expected: &BigUintFp2) {
    assert_eq!(BigUint::from(got.real), expected.real, "real mismatch");
    assert_eq!(BigUint::from(got.img), expected.img, "imag mismatch");
}

#[test]
fn fp2m61_constants_and_properties() {
    let p = BigUint::from(Fp2M61::MODULUS_P);
    assert_eq!(p, (BigUint::one() << 61usize) - BigUint::one());
    assert_eq!(p.bits(), 61);

    // Cardinality p^2 = (2^61 - 1)^2 lies in (2^121, 2^122): CLOG2 = 122, FLOG2 = 121
    assert_eq!(<Fp2M61 as SizedField>::CLOG2, 122);
    assert_eq!(<Fp2M61 as SizedField>::FLOG2, 121);

    assert_eq!(Fp2M61::MAX_ACCUMULATIONS, 32);

    // Identities
    assert_eq!(Fp2M61::ZERO.real, 0);
    assert_eq!(Fp2M61::ZERO.img, 0);
    assert_eq!(Fp2M61::ONE.real, 1);
    assert_eq!(Fp2M61::ONE.img, 0);
    assert_eq!(Fp2M61::I.real, 0);
    assert_eq!(Fp2M61::I.img, 1);

    // i^2 = -1 = p - 1
    assert_eq!(Fp2M61::I.square(), -Fp2M61::ONE);
    assert_eq!(Fp2M61::I.square().real, Fp2M61::MODULUS_P - 1);
    assert_eq!(Fp2M61::I.square().img, 0);

    // prepare_weight is identity
    let w = Fp2M61::from_parts(12345, 67890);
    assert_eq!(Fp2M61::prepare_weight(&w), w);

    // FieldHash produces 16 little-endian bytes
    let hrepr = w.to_hash_repr();
    assert_eq!(hrepr.as_ref().len(), 16);
    assert_eq!(Fp2M61::from_repr(hrepr).unwrap(), w);

    // NON_SQUARE norm is 3
    let norm_ns = (Fp2M61::NON_SQUARE.real as u128) * (Fp2M61::NON_SQUARE.real as u128)
        + (Fp2M61::NON_SQUARE.img as u128) * (Fp2M61::NON_SQUARE.img as u128);
    let norm_ns_reduced = (norm_ns % (Fp2M61::MODULUS_P as u128)) as u64;
    assert_eq!(norm_ns_reduced, 3);
    assert!(bool::from(Fp2M61::NON_SQUARE.sqrt().is_none()));
}

#[test]
fn fp2m61_axioms() {
    let mut rng = ChaCha20Rng::seed_from_u64(0x2026_0612);

    assert_eq!(Fp2M61::ZERO + Fp2M61::ZERO, Fp2M61::ZERO);
    assert_eq!(Fp2M61::ZERO - Fp2M61::ZERO, Fp2M61::ZERO);
    assert_eq!(-Fp2M61::ZERO, Fp2M61::ZERO);
    assert_eq!(Fp2M61::ZERO * Fp2M61::ONE, Fp2M61::ZERO);
    assert_eq!(Fp2M61::ONE * Fp2M61::ONE, Fp2M61::ONE);
    assert!(bool::from(ff::Field::is_zero(&Fp2M61::ZERO)));
    assert!(!bool::from(ff::Field::is_zero(&Fp2M61::ONE)));
    assert!(bool::from(Fp2M61::ZERO.invert().is_none()));
    assert_eq!(Fp2M61::ONE.invert().unwrap(), Fp2M61::ONE);

    for _ in 0..1_000 {
        let x = Fp2M61::random(&mut rng);
        let y = Fp2M61::random(&mut rng);
        let z = Fp2M61::random(&mut rng);

        // Additive axioms
        assert_eq!(x + Fp2M61::ZERO, x);
        assert_eq!(Fp2M61::ZERO + x, x);
        assert_eq!(x + (-x), Fp2M61::ZERO);
        assert_eq!(x - x, Fp2M61::ZERO);
        assert_eq!(x + y, y + x);
        assert_eq!((x + y) + z, x + (y + z));
        assert_eq!(x.double(), x + x);

        // Multiplicative axioms
        assert_eq!(x * Fp2M61::ONE, x);
        assert_eq!(Fp2M61::ONE * x, x);
        assert_eq!(x * Fp2M61::ZERO, Fp2M61::ZERO);
        assert_eq!(x * y, y * x);
        assert_eq!((x * y) * z, x * (y * z));
        assert_eq!(x.square(), x * x);
        assert_eq!(x.cube(), x * x * x);

        // Distributivity
        assert_eq!(x * (y + z), x * y + x * z);
        assert_eq!((x - y) * z, x * z - y * z);

        // Serialization round-trip
        let repr = x.to_repr();
        let recovered = Fp2M61::from_repr(repr).unwrap();
        assert_eq!(x, recovered);

        let bin = bincode::serialize(&x).unwrap();
        let deser: Fp2M61 = bincode::deserialize(&bin).unwrap();
        assert_eq!(x, deser);

        // Inversion
        if !bool::from(ff::Field::is_zero(&x)) {
            let inv = x.invert().unwrap();
            assert_eq!(x * inv, Fp2M61::ONE);
        }

        // Square and square root
        let sq = x.square();
        let root = sq.sqrt().unwrap();
        assert_eq!(root.square(), sq);

        // sqrt_ratio
        if !bool::from(ff::Field::is_zero(&y)) {
            let (is_sq, r) = Fp2M61::sqrt_ratio(&x, &y);
            if bool::from(is_sq) {
                assert_eq!(r.square(), x / y);
            } else {
                assert_eq!(r.square(), Fp2M61::NON_SQUARE * (x / y));
            }
        }
    }

    // Num trait checks
    assert_eq!(<Fp2M61 as Num>::from_str_radix("0", 10).unwrap(), Fp2M61::ZERO);
    assert_eq!(<Fp2M61 as Num>::from_str_radix("1", 10).unwrap(), Fp2M61::ONE);
    assert_eq!(
        <Fp2M61 as Num>::from_str_radix("12345678", 16).unwrap(),
        Fp2M61::from(0x12345678u64)
    );
}

#[test]
fn fp2m61_differential_vs_bigint() {
    let mut rng = ChaCha20Rng::seed_from_u64(0x3333_4444);
    let p = Fp2M61::MODULUS_P;

    // Edge-case coordinates: 0, 1, 2, p-2, p-1, 2^31, 2^60
    let edge_coords: Vec<u64> = vec![
        0,
        1,
        2,
        p - 2,
        p - 1,
        1 << 31,
        1 << 60,
    ];

    let mut edge_elems = Vec::new();
    for &r in &edge_coords {
        for &i in &edge_coords {
            edge_elems.push(Fp2M61::from_parts(r, i));
        }
    }

    // All pairs of edge cases
    for a in &edge_elems {
        let a_big = BigUintFp2::from_elem(a);
        assert_fp2_eq_biguint(&(-*a), &a_big.neg());
        assert_fp2_eq_biguint(&a.double(), &a_big.double());
        assert_fp2_eq_biguint(&a.square(), &a_big.square());

        for b in &edge_elems {
            let b_big = BigUintFp2::from_elem(b);
            let add_exp = a_big.add(&b_big);
            let sub_exp = a_big.sub(&b_big);
            let mul_exp = a_big.mul(&b_big);

            assert_fp2_eq_biguint(&(*a + *b), &add_exp);
            assert_fp2_eq_biguint(&(*a - *b), &sub_exp);
            assert_fp2_eq_biguint(&(*a * *b), &mul_exp);

            // Single-edge LazyField::mac + reduce
            let mut acc = Fp2M61Acc::default();
            let w_prep = Fp2M61::prepare_weight(a);
            Fp2M61::mac(&mut acc, &w_prep, b);
            let reduced = Fp2M61::reduce(acc);
            assert_fp2_eq_biguint(&reduced, &mul_exp);
        }
    }

    // 3,000 random pairs vs BigUint
    for _ in 0..3_000 {
        let a = Fp2M61::random(&mut rng);
        let b = Fp2M61::random(&mut rng);
        let a_big = BigUintFp2::from_elem(&a);
        let b_big = BigUintFp2::from_elem(&b);

        assert_fp2_eq_biguint(&(a + b), &a_big.add(&b_big));
        assert_fp2_eq_biguint(&(a - b), &a_big.sub(&b_big));
        assert_fp2_eq_biguint(&(a * b), &a_big.mul(&b_big));
        assert_fp2_eq_biguint(&a.square(), &a_big.square());
    }

    // Multi-edge lazy MAC + fold + reduce vs BigUint across degrees 1..=200
    for deg in [1usize, 2, 7, 16, 31, 32, 33, 64, 128, 200] {
        let mut acc = Fp2M61Acc::default();
        let mut sum_big = BigUintFp2::new(BigUint::zero(), BigUint::zero());
        for e in 0..deg {
            let w = Fp2M61::random(&mut rng);
            let x = Fp2M61::random(&mut rng);
            let w_big = BigUintFp2::from_elem(&w);
            let x_big = BigUintFp2::from_elem(&x);
            sum_big = sum_big.add(&w_big.mul(&x_big));

            let w_prep = Fp2M61::prepare_weight(&w);
            Fp2M61::mac(&mut acc, &w_prep, &x);
            // Chunking: fold every MAX_ACCUMULATIONS edges
            if (e + 1) % Fp2M61::MAX_ACCUMULATIONS == 0 {
                Fp2M61::fold(&mut acc);
            }
        }
        let got = Fp2M61::reduce(acc);
        assert_fp2_eq_biguint(&got, &sum_big);
    }
}

#[test]
fn fp2m61_max_accumulator_fill_and_boundaries() {
    let p = Fp2M61::MODULUS_P;
    let pm1 = Fp2M61::from_parts(p - 1, p - 1);
    let pm1_big = BigUintFp2::from_elem(&pm1);

    // 1. Accumulate exactly 32 maximal products (p - 1 + i(p - 1))^2 without intermediate fold
    let w_prep = Fp2M61::prepare_weight(&pm1);
    let mut acc = Fp2M61Acc::default();
    let count = 32usize;
    let mut sum_big = BigUintFp2::new(BigUint::zero(), BigUint::zero());
    let prod_big = pm1_big.mul(&pm1_big);

    for _ in 0..count {
        Fp2M61::mac(&mut acc, &w_prep, &pm1);
        sum_big = sum_big.add(&prod_big);
    }
    let reduced = Fp2M61::reduce(acc);
    assert_fp2_eq_biguint(&reduced, &sum_big);

    // 2. Accumulate 16 into acc1, 16 into acc2, add, reduce
    let mut acc1 = Fp2M61Acc::default();
    let mut acc2 = Fp2M61Acc::default();
    for _ in 0..16 {
        Fp2M61::mac(&mut acc1, &w_prep, &pm1);
        Fp2M61::mac(&mut acc2, &w_prep, &pm1);
    }
    Fp2M61::add(&mut acc1, &acc2);
    assert_eq!(Fp2M61::reduce(acc1), reduced);

    // 3. Mathematical limit boundary test:
    // Fold resets accumulator to < 2^62, allowing another 31 edges
    let mut acc_chain = Fp2M61Acc::default();
    for _ in 0..32 {
        Fp2M61::mac(&mut acc_chain, &w_prep, &pm1);
    }
    Fp2M61::fold(&mut acc_chain);
    assert!(acc_chain.re < (1u128 << 62));
    assert!(acc_chain.im < (1u128 << 62));
    for _ in 0..32 {
        Fp2M61::mac(&mut acc_chain, &w_prep, &pm1);
    }
    let red_chain = Fp2M61::reduce(acc_chain);
    let mut expected_64 = BigUintFp2::new(BigUint::zero(), BigUint::zero());
    for _ in 0..64 {
        expected_64 = expected_64.add(&prod_big);
    }
    assert_fp2_eq_biguint(&red_chain, &expected_64);
}

#[test]
fn fp2m61_simd_spielman_test_vectors_and_encoding() {
    use std::path::Path;

    // Same lookup as `lcpc_spielman_oracle::locate_tv_dir`: the
    // `SIMD_SPIELMAN_TV_DIR` override first, then the workspace layout.
    let tv_dir = match std::env::var_os("SIMD_SPIELMAN_TV_DIR") {
        Some(dir) => Path::new(&dir).to_path_buf(),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../SIMD-Spielman-for-ZKPs/tv"),
    };
    let tv_in_path = tv_dir.join("bd_spielman_extension_N12_in.txt");

    let content = std::fs::read_to_string(&tv_in_path).unwrap_or_else(|error| {
        panic!("required SIMD-Spielman test vector {:?} is unavailable: {error}", tv_in_path)
    });
    let p = Fp2M61::MODULUS_P;
    let mut elems = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(parts.len(), 2, "Each line must have real and img hex components");
        let real = u64::from_str_radix(parts[0], 16).unwrap();
        let img = u64::from_str_radix(parts[1], 16).unwrap();

        let elem = Fp2M61::from_parts(real, img);
        assert!(elem.is_valid());

        // Check format round-trip matching SIMD-Spielman's printf("%lx %lx\n")
        let formatted = format!("{:x} {:x}", elem.real, elem.img);
        assert_eq!(formatted, line);

        elems.push(elem);
    }

    assert_eq!(elems.len(), 4096);

    // Verify arithmetic matches SIMD-Spielman's ExtensionField_scalar formulas
    for i in 0..200 {
        let a = elems[i];
        let b = elems[(i + 1) % elems.len()];

        // Addition: (a + b) % ORION_PRIME
        let exp_add_re = (a.real + b.real) % p;
        let exp_add_im = (a.img + b.img) % p;
        assert_eq!((a + b).real, exp_add_re);
        assert_eq!((a + b).img, exp_add_im);

        // Subtraction: (ORION_PRIME + a - b) % ORION_PRIME
        let exp_sub_re = (p + a.real - b.real) % p;
        let exp_sub_im = (p + a.img - b.img) % p;
        assert_eq!((a - b).real, exp_sub_re);
        assert_eq!((a - b).img, exp_sub_im);

        // Multiplication: ExtensionField_scalar::operator*
        let real_mul_1 = ((a.real as u128) * (b.real as u128)) % (p as u128);
        let real_mul_2 = ((a.img as u128) * (b.img as u128)) % (p as u128);
        let exp_mul_re = ((p as u128 + real_mul_1 - real_mul_2) % (p as u128)) as u64;
        let exp_mul_im = (((a.real as u128) * (b.img as u128) + (a.img as u128) * (b.real as u128))
            % (p as u128)) as u64;

        let got_mul = a * b;
        assert_eq!(got_mul.real, exp_mul_re);
        assert_eq!(got_mul.img, exp_mul_im);

        // Also check single-edge LazyField::mac + reduce
        let mut acc = Fp2M61Acc::default();
        let w_prep = Fp2M61::prepare_weight(&a);
        Fp2M61::mac(&mut acc, &w_prep, &b);
        let red = Fp2M61::reduce(acc);
        assert_eq!(red, got_mul);
    }
}

#[test]
fn fp2m61_column_kernels_match_scalar_oracle() {
    let mut rng = ChaCha20Rng::seed_from_u64(0x5555_6666);
    let rows = 64usize;

    let w = Fp2M61::random(&mut rng);
    let w_prep = Fp2M61::prepare_weight(&w);
    let src: Vec<Fp2M61> = (0..rows).map(|_| Fp2M61::random(&mut rng)).collect();
    let mut acc = vec![Fp2M61Acc::default(); rows];
    let mut dst = vec![Fp2M61::ZERO; rows];

    mac_column_fp2_m61(&mut acc, &w_prep, &src);
    fold_column_fp2_m61(&mut acc);
    mac_column_fp2_m61(&mut acc, &w_prep, &src);
    reduce_column_fp2_m61(&mut dst, &mut acc);

    for i in 0..rows {
        let expected = (w * src[i]).double();
        assert_eq!(dst[i], expected);
        assert_eq!(acc[i], Fp2M61Acc::default());
    }
}
