// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.
//
// Modified 2026 by the RealBrakedownPerf authors (Apache-2.0, see NOTICE):
// runtime code parameters (LCPC mode (a), SIMD-Spielman mode (b)).
// The per-commit changes are in the git history since step/00-import
// (upstream conroi/lcpc f0e2116).

/*! specify code parameters */

use std::fmt::Debug;
use std::marker::{PhantomData, Sync};
use std::ops::Add;
use typenum::Unsigned;

fn ent(z: f64) -> f64 {
    assert!(0f64 < z && z < 1f64);
    let mzp1 = 1f64 - z;
    -z * z.log2() - mzp1 * mzp1.log2()
}

/// Specify an SDIG code
pub trait SdigSpecification {
    /// numerator of alpha
    type An: Unsigned;
    /// denominator of alpha
    type Ad: Unsigned;

    /// numerator of beta
    type Bn: Unsigned;
    /// denominator of beta
    type Bd: Unsigned;

    /// numerator of R
    type Rn: Unsigned;
    /// denominator of R
    type Rd: Unsigned;

    /// base-case code length
    type Blen: Unsigned;

    /// distance as f64 {
    fn dist() -> f64 {
        (Self::Bn::to_usize() * Self::Rd::to_usize()) as f64
            / (Self::Bd::to_usize() * Self::Rn::to_usize()) as f64
    }

    /// alpha num as usize
    fn alpha_num() -> usize {
        Self::An::to_usize()
    }

    /// alpha den as usize
    fn alpha_den() -> usize {
        Self::Ad::to_usize()
    }

    /// beta num as usize
    fn beta_num() -> usize {
        Self::Bn::to_usize()
    }

    /// beta den as usize
    fn beta_den() -> usize {
        Self::Bd::to_usize()
    }

    /// r num as usize
    fn r_num() -> usize {
        Self::Rn::to_usize()
    }

    /// r den as usize
    fn r_den() -> usize {
        Self::Rd::to_usize()
    }

    /// baselen
    fn baselen() -> usize {
        Self::Blen::to_usize()
    }

    /// alpha as f64
    fn alpha() -> f64 {
        Self::An::to_usize() as f64 / Self::Ad::to_usize() as f64
    }

    /// beta as f64
    fn beta() -> f64 {
        Self::Bn::to_usize() as f64 / Self::Bd::to_usize() as f64
    }

    /// r as f64
    fn r() -> f64 {
        Self::Rn::to_usize() as f64 / Self::Rd::to_usize() as f64
    }

    /// mu = r - 1 - r * alpha
    fn mu() -> f64 {
        Self::r() - 1f64 - Self::r() * Self::alpha()
    }

    /// nu = beta + alpha * beta + 0.03
    fn nu() -> f64 {
        Self::beta() + Self::alpha() * Self::beta() + 0.03f64
    }

    /// constant for cn calculation
    fn cnst_cn_1() -> f64 {
        ent(Self::beta()) + Self::alpha() * ent(1.28f64 * Self::beta() / Self::alpha())
    }

    /// constant for cn calculation
    fn cnst_cn_2() -> f64 {
        Self::beta() * (Self::alpha() / (1.28f64 * Self::beta())).log2()
    }

    /// constant for dn calculation
    fn cnst_dn_1() -> f64 {
        Self::r() * Self::alpha() * ent(Self::beta() / Self::r())
            + Self::mu() * ent(Self::nu() / Self::mu())
    }

    /// constant for dn calculation
    fn cnst_dn_2() -> f64 {
        Self::alpha() * Self::beta() * (Self::mu() / Self::nu()).log2()
    }
}

/// A concrete SdigSpecification object
#[derive(Clone, Debug)]
pub struct SdigSpec<An, Ad, Bn, Bd, Rn, Rd, Blen>
where
    An: Unsigned + Debug + Sync,
    Ad: Unsigned + Debug + Sync,
    Bn: Unsigned + Debug + Sync,
    Bd: Unsigned + Debug + Sync,
    Rn: Unsigned + Debug + Sync,
    Rd: Unsigned + Debug + Sync,
    Blen: Unsigned + Debug + Sync,
{
    _p: PhantomData<(An, Ad, Bn, Bd, Rn, Rd, Blen)>,
}

impl<An, Ad, Bn, Bd, Rn, Rd, Blen> SdigSpecification for SdigSpec<An, Ad, Bn, Bd, Rn, Rd, Blen>
where
    An: Unsigned + Debug + Sync,
    Ad: Unsigned + Debug + Sync,
    Bn: Unsigned + Debug + Sync,
    Bd: Unsigned + Debug + Sync,
    Rn: Unsigned + Debug + Sync,
    Rd: Unsigned + Debug + Sync,
    Blen: Unsigned + Debug + Sync,
{
    type An = An;
    type Ad = Ad;
    type Bn = Bn;
    type Bd = Bd;
    type Rn = Rn;
    type Rd = Rd;
    type Blen = Blen;
}

type U1521 = <typenum::U1021 as Add<typenum::U500>>::Output;
type U2000 = <typenum::U1000 as Add<typenum::U1000>>::Output;
type U2500 = <typenum::U500 as Add<U2000>>::Output;
/// line 1 from table
pub type SdigCode1 = SdigSpec<
    typenum::U239, // alpha = 0.1195
    U2000,
    typenum::U71, // beta = 0.0284
    U2500,
    typenum::U71, // r = 1.42
    typenum::U50,
    typenum::U20, // baselen = 20
>;

/// line 2 from table
pub type SdigCode2 = SdigSpec<
    typenum::U69, // alpha = 0.138
    typenum::U500,
    typenum::U111, // beta = 0.0444
    U2500,
    typenum::U147, // r = 1.47
    typenum::U100,
    typenum::U20, // baselen = 20
>;

/// line 3 from table
pub type SdigCode3 = SdigSpec<
    typenum::U89, // alpha = 0.178
    typenum::U500,
    typenum::U61, // beta = 0.061
    typenum::U1000,
    U1521, // r = 1.521
    typenum::U1000,
    typenum::U20, // baselen = 20
>;

/// line 4 from table
pub type SdigCode4 = SdigSpec<
    typenum::U1, // alpha = 0.2
    typenum::U5,
    typenum::U41, // beta = 0.082
    typenum::U500,
    typenum::U41, // r = 1.64
    typenum::U25,
    typenum::U20, // baselen = 20
>;

/// line 5 from table
pub type SdigCode5 = SdigSpec<
    typenum::U211, // alpha = 0.211
    typenum::U1000,
    typenum::U97, // beta = 0.097
    typenum::U1000,
    typenum::U202, // r = 1.616
    typenum::U125,
    typenum::U20, // baselen = 20
>;

/// line 6 from table
pub type SdigCode6 = SdigSpec<
    typenum::U119, // alpha = 0.238
    typenum::U500,
    typenum::U241, // beta = 0.1205
    U2000,
    typenum::U43, // r = 1.72
    typenum::U25,
    typenum::U20, // baselen = 20
>;

// ---------------------------------------------------------------------------
// Runtime code parameters
// ---------------------------------------------------------------------------

/// A snapshot of an [`SdigSpecification`] as runtime values.
///
/// Every number that LCPC's dimension recurrence (`matgen::get_dims`) and
/// column-opening count read from the specification is captured by calling the
/// specification's own methods, so a code built from `LcpcParams::of::<S>()` is
/// identical to one built from `S` directly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LcpcParams {
    alpha_num: usize,
    alpha_den: usize,
    beta_num: usize,
    beta_den: usize,
    r_num: usize,
    r_den: usize,
    baselen: usize,
    dist: f64,
    cnst_cn_1: f64,
    cnst_cn_2: f64,
    cnst_dn_1: f64,
    cnst_dn_2: f64,
}

impl LcpcParams {
    /// Capture the parameters of specification `S`.
    pub fn of<S: SdigSpecification>() -> Self {
        Self {
            alpha_num: S::alpha_num(),
            alpha_den: S::alpha_den(),
            beta_num: S::beta_num(),
            beta_den: S::beta_den(),
            r_num: S::r_num(),
            r_den: S::r_den(),
            baselen: S::baselen(),
            dist: S::dist(),
            cnst_cn_1: S::cnst_cn_1(),
            cnst_cn_2: S::cnst_cn_2(),
            cnst_dn_1: S::cnst_dn_1(),
            cnst_dn_2: S::cnst_dn_2(),
        }
    }

    /// numerator of alpha
    pub fn alpha_num(&self) -> usize {
        self.alpha_num
    }

    /// denominator of alpha
    pub fn alpha_den(&self) -> usize {
        self.alpha_den
    }

    /// numerator of beta
    pub fn beta_num(&self) -> usize {
        self.beta_num
    }

    /// denominator of beta
    pub fn beta_den(&self) -> usize {
        self.beta_den
    }

    /// numerator of r
    pub fn r_num(&self) -> usize {
        self.r_num
    }

    /// denominator of r
    pub fn r_den(&self) -> usize {
        self.r_den
    }

    /// base-case code length (recursion stops once a level has at most this many inputs)
    pub fn baselen(&self) -> usize {
        self.baselen
    }

    /// relative distance (`SdigSpecification::dist`)
    pub fn dist(&self) -> f64 {
        self.dist
    }

    /// constant for cn calculation (`SdigSpecification::cnst_cn_1`)
    pub fn cnst_cn_1(&self) -> f64 {
        self.cnst_cn_1
    }

    /// constant for cn calculation (`SdigSpecification::cnst_cn_2`)
    pub fn cnst_cn_2(&self) -> f64 {
        self.cnst_cn_2
    }

    /// constant for dn calculation (`SdigSpecification::cnst_dn_1`)
    pub fn cnst_dn_1(&self) -> f64 {
        self.cnst_dn_1
    }

    /// constant for dn calculation (`SdigSpecification::cnst_dn_2`)
    pub fn cnst_dn_2(&self) -> f64 {
        self.cnst_dn_2
    }
}

/// The fixed code parameters of SIMD-Spielman (`spielman/SpielmanParams.h` at
/// commit `7b399db`), which follow Orion and Brakedown.
#[derive(Clone, Copy, Debug)]
pub struct SimdSpielmanParams;

impl SimdSpielmanParams {
    /// Target relative distance `code_distance = 0.07`.
    pub const CODE_DISTANCE: f64 = 0.07;
    /// Recursion threshold `(int)(1.0 / code_distance) - 1 = 13`: a level with
    /// fewer left nodes is not expanded (identity base case).
    pub const DISTANCE_THRESHOLD: usize = (1.0 / Self::CODE_DISTANCE) as usize - 1;
    /// Precode ratio `alpha = 0.238` (right nodes `floor(alpha * n)`).
    pub const ALPHA: f64 = 0.238;
    /// `beta = 0.1205` (only enters through the relative distance).
    pub const BETA: f64 = 0.1205;
    /// Inverse rate `r = 1.72`.
    pub const R: f64 = 1.72;
    /// Left degree of every precode graph.
    pub const CN: usize = 10;
    /// Left degree of every postcode graph.
    pub const DN: usize = 20;
}

/// Code-parameter mode, chosen at runtime.
///
/// This selects *which code* is computed, not how it is computed: both modes
/// use the same encoder (R2L graphs, column-major batches). The mode is
/// recorded with every benchmark result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CodeParams {
    /// Mode (a): LCPC's `SdigSpecification` code. Dimensions by exact rational
    /// ceiling division, per-level degrees `c_n`, `d_n` from the entropy formulas,
    /// distinct neighbors per left node, recursion down to `baselen` and a
    /// Reed-Solomon base case. Bit-exact with upstream LCPC `f0e2116`.
    Lcpc(LcpcParams),
    /// Mode (b): SIMD-Spielman's code (`SpielmanParams.h`): `alpha = 0.238`,
    /// `r = 1.72` with floating-point floor dimensions, fixed degrees
    /// `c_n = 10`, `d_n = 20`, neighbors sampled with replacement (parallel
    /// edges kept), recursion threshold 13 and an identity base case.
    SimdSpielman,
}

impl CodeParams {
    /// Mode (a) with `SdigCode6` (alpha = 0.238, beta = 0.1205, r = 1.72).
    pub fn lcpc_sdig6() -> Self {
        CodeParams::Lcpc(LcpcParams::of::<SdigCode6>())
    }

    /// Mode (a) with `SdigCode3`, LCPC's default code.
    pub fn lcpc_sdig3() -> Self {
        CodeParams::Lcpc(LcpcParams::of::<SdigCode3>())
    }

    /// Parse a mode name: `lcpc-sdig6`, `lcpc-sdig3`, or `simd-spielman`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "lcpc-sdig6" => Some(Self::lcpc_sdig6()),
            "lcpc-sdig3" => Some(Self::lcpc_sdig3()),
            "simd-spielman" => Some(CodeParams::SimdSpielman),
            _ => None,
        }
    }

    /// The mode name accepted by [`CodeParams::from_name`], or `lcpc-custom`
    /// for another LCPC specification.
    pub fn name(&self) -> &'static str {
        match self {
            CodeParams::Lcpc(p) if *p == LcpcParams::of::<SdigCode6>() => "lcpc-sdig6",
            CodeParams::Lcpc(p) if *p == LcpcParams::of::<SdigCode3>() => "lcpc-sdig3",
            CodeParams::Lcpc(_) => "lcpc-custom",
            CodeParams::SimdSpielman => "simd-spielman",
        }
    }

    /// Relative distance used to size the column openings of the PCS.
    ///
    /// Mode (b) uses the same `beta / r` as `SdigCode6` (whose `alpha`, `beta`
    /// and `r` it shares), so both modes open the same number of columns.
    pub fn dist(&self) -> f64 {
        match self {
            CodeParams::Lcpc(p) => p.dist(),
            CodeParams::SimdSpielman => SdigCode6::dist(),
        }
    }
}
