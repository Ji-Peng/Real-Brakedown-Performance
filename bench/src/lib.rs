// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Benchmark and evaluation harness library for the LCPC workspace.

use std::fmt::Debug;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as IoWrite;
use std::path::Path;
use std::time::Instant;

use digest::{FixedOutput, Output, Update};
use ff::{Field, PrimeField};
use lcpc_2d::{FieldHash, LcCommit, LcEncoding, SizedField};
use lcpc_brakedown_pc::codespec::{CodeParams, SdigCode3, SdigCode6, SdigSpecification};
use lcpc_brakedown_pc::{SdigEncodingS, SpielmanEncoding};
use lcpc_fields::{Fp2M61, LazyField, Mers127, Mont127};
use lcpc_test_fields::{ft127::Ft127, ft63::Ft63, Blake3};
use merlin::Transcript;
use num_traits::Num;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const UPSTREAM_COMMIT: &str = "f0e2116001af5a1bbf39481535f837d6a25772e8";
pub const HASH_SEED: u64 = 0x4c43_5043_4f52_4947;
pub const HASH_MODE: &str = "portable-only";

fn default_shape() -> String {
    "brakedown".to_string()
}

fn default_status() -> String {
    "complete".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub min_ns: u128,
    pub q1_ns: u128,
    pub median_ns: f64,
    pub q3_ns: u128,
    pub iqr_ns: u128,
    pub max_ns: u128,
    pub mean_ns: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Samples {
    pub samples_ns: Vec<u128>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dimensions {
    pub coefficient_count: usize,
    pub n_rows: usize,
    pub n_per_row: usize,
    pub n_cols: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelParameters {
    pub n_i: usize,
    pub m_i: usize,
    pub c_n: usize,
    pub n_i_prime: usize,
    pub m_i_prime: usize,
    pub d_n: usize,
    #[serde(default)]
    pub precode_edges: usize,
    #[serde(default)]
    pub postcode_edges: usize,
    #[serde(default)]
    pub total_edges: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupTimes {
    pub graph_sampling_and_encoding_init_ns: u128,
    pub input_generation_and_row_buffer_init_ns: u128,
    pub verifier_encoding_init_ns: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurements {
    pub encode_matrix: Samples,
    pub verifier_encode: Samples,
    #[serde(default)]
    pub verifier_encode_1: Option<Samples>,
    #[serde(default)]
    pub verifier_encode_2: Option<Samples>,
    pub hash_matrix_decomposition: Samples,
    pub commit_total: Samples,
    pub prove: Samples,
    pub verify: Samples,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correctness {
    pub direct_encoder_root_matches_commit: bool,
    pub proof_verification_passed: bool,
    pub verified_evaluation_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerfStatSummary {
    pub cycles: Option<u64>,
    pub instructions: Option<u64>,
    pub ipc: Option<f64>,
    pub branch_misses: Option<u64>,
    pub cache_misses: Option<u64>,
    pub llc_misses: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub schema_version: String,
    pub implementation: String,
    pub upstream_commit: String,
    pub driver_version: String,
    pub hash_algorithm: String,
    pub hash_crate_version: String,
    pub hash_mode: String,
    pub field: String,
    pub params: String,
    #[serde(default = "default_shape")]
    pub shape: String,
    pub log_n: u32,
    pub seed: u64,
    pub threads: usize,
    pub cores: String,
    pub slices: usize,
    pub slice_mode: String,
    #[serde(default)]
    pub adaptive_slices: usize,
    pub warmup: usize,
    pub runs: usize,
    pub dimensions: Dimensions,
    pub recursion_depth: usize,
    pub level_parameters: Vec<LevelParameters>,
    #[serde(default)]
    pub total_graph_edges: usize,
    #[serde(default)]
    pub total_edge_row_macs: usize,
    #[serde(default)]
    pub ns_per_mac: f64,
    #[serde(default)]
    pub cycles_per_mac: Option<f64>,
    #[serde(default)]
    pub cpu_mhz_before: Option<f64>,
    #[serde(default)]
    pub cpu_mhz_after: Option<f64>,
    /// Where `cpu_mhz_before`/`cpu_mhz_after` (and so `cycles_per_mac`) come
    /// from; see [`CPU_MHZ_SOURCE`].
    #[serde(default)]
    pub cpu_mhz_source: Option<String>,
    /// Always `true` when `cycles_per_mac` is present: it is derived from the
    /// `/proc/cpuinfo` MHz snapshot of [`CPU_MHZ_SOURCE`], not from a cycle
    /// counter or the pinned core's measured clock.
    #[serde(default)]
    pub cycles_per_mac_is_nominal: Option<bool>,
    #[serde(default)]
    pub proof_size_bytes: usize,
    pub setup_ns: SetupTimes,
    pub measurements: Measurements,
    pub correctness: Correctness,
    pub peak_rss_bytes: usize,
    pub hash_decomposition_includes_scratch_allocation: bool,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub perf_stat: Option<PerfStatSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldOpSummary {
    pub op: &'static str,
    pub iterations: usize,
    pub total_ns: u128,
    pub ns_per_op: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldMicrobenchReport {
    pub field: &'static str,
    pub mul: FieldOpSummary,
    pub mac: FieldOpSummary,
    pub reduce: FieldOpSummary,
}

#[derive(Serialize)]
pub struct GoldenFile {
    pub schema_version: &'static str,
    pub generator: &'static str,
    pub upstream_commit: &'static str,
    pub hash_algorithm: &'static str,
    pub hash_crate_version: &'static str,
    pub cases: Vec<GoldenCase>,
}

#[derive(Serialize, PartialEq, Eq)]
pub struct GoldenCase {
    pub case_id: String,
    pub params: &'static str,
    pub field: &'static str,
    pub seed: u64,
    pub evaluation_seed: u64,
    pub n_rows: usize,
    pub n_per_row: usize,
    pub n_cols: usize,
    pub coefficients_hex: Vec<String>,
    pub encoded_rows_hex: Vec<Vec<String>>,
    pub commitment_root_hex: String,
    pub proof_bincode_blake3_hex: String,
    pub proof_bincode_bytes: usize,
    pub proof_verified: bool,
    pub verified_evaluation_hex: String,
}

/// Reads current peak resident set size in bytes on Linux from `/proc/self/status`.
pub fn get_peak_rss_bytes() -> usize {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("VmHWM:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(kb) = parts[1].parse::<usize>() {
                        return kb * 1024;
                    }
                }
            }
        }
        for line in status.lines() {
            if line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(kb) = parts[1].parse::<usize>() {
                        return kb * 1024;
                    }
                }
            }
        }
    }
    0
}

pub fn sample_times(mut runs: usize, mut operation: impl FnMut()) -> Vec<u128> {
    let mut samples = Vec::with_capacity(runs);
    while runs > 0 {
        let start = Instant::now();
        operation();
        samples.push(start.elapsed().as_nanos());
        runs -= 1;
    }
    samples
}

pub fn summarize(samples: Vec<u128>) -> Samples {
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    let median_ns = if sorted.len() % 2 == 0 {
        (sorted[middle - 1] as f64 + sorted[middle] as f64) / 2.0
    } else {
        sorted[middle] as f64
    };
    let mean_ns = samples.iter().map(|v| *v as f64).sum::<f64>() / samples.len() as f64;
    let q1_ns = sorted[(sorted.len() - 1) / 4];
    let q3_ns = sorted[3 * (sorted.len() - 1) / 4];
    let iqr_ns = q3_ns.saturating_sub(q1_ns);
    let summary = Summary {
        min_ns: sorted[0],
        q1_ns,
        median_ns,
        q3_ns,
        iqr_ns,
        max_ns: *sorted.last().unwrap(),
        mean_ns,
    };
    Samples {
        samples_ns: samples,
        summary,
    }
}

pub fn write_output(path: &str, text: &str) -> Result<(), String> {
    let path = Path::new(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let mut file = fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn params_tag(params: &CodeParams) -> u64 {
    match params.name() {
        "lcpc-sdig6" => 6,
        "lcpc-sdig3" => 3,
        "simd-spielman" => 0x5353,
        _ => 0,
    }
}

pub fn encode_matrix<F, S>(enc: &S, matrix: &mut [F], n_cols: usize)
where
    S: LcEncoding<F = F>,
{
    enc.encode_rows(matrix, n_cols).expect("LCPC encode failed");
}

pub fn hash_matrix<F>(matrix: &[F], n_rows: usize, n_cols: usize) -> Output<Blake3>
where
    F: FieldHash + Sync,
{
    let leaf_count = n_cols.next_power_of_two();
    let mut leaves: Vec<Output<Blake3>> = (0..leaf_count)
        .into_par_iter()
        .map(|col| {
            if col >= n_cols {
                return Output::<Blake3>::default();
            }
            let mut digest = Blake3::default();
            Update::update(&mut digest, Output::<Blake3>::default().as_slice());
            for row in 0..n_rows {
                matrix[row * n_cols + col].digest_update(&mut digest);
            }
            digest.finalize_fixed()
        })
        .collect();
    while leaves.len() > 1 {
        leaves = leaves
            .par_chunks_exact(2)
            .map(|pair| {
                let mut digest = Blake3::default();
                Update::update(&mut digest, pair[0].as_slice());
                Update::update(&mut digest, pair[1].as_slice());
                digest.finalize_fixed()
            })
            .collect();
    }
    leaves.pop().unwrap_or_default()
}

pub fn make_transcript(root: &Output<Blake3>, n_col_opens: usize) -> Transcript {
    let mut transcript = Transcript::new(b"LCPC original baseline");
    transcript.append_message(b"polycommit", root.as_slice());
    transcript.append_message(b"ncols", &(n_col_opens as u64).to_be_bytes());
    transcript
}

pub fn tensors<F: Field>(point: F, n_per_row: usize, n_rows: usize) -> (Vec<F>, Vec<F>) {
    let mut inner = Vec::with_capacity(n_per_row);
    let mut value = F::ONE;
    for _ in 0..n_per_row {
        inner.push(value);
        value *= point;
    }
    let row_step = value;
    let mut outer = Vec::with_capacity(n_rows);
    value = F::ONE;
    for _ in 0..n_rows {
        outer.push(value);
        value *= row_step;
    }
    (inner, outer)
}

pub fn evaluate_polynomial_tensors<F: Field>(coefficients: &[F], inner: &[F], outer: &[F]) -> F {
    coefficients
        .chunks(inner.len())
        .zip(outer)
        .fold(F::ZERO, |acc, (row, row_weight)| {
            let row_value = row
                .iter()
                .zip(inner)
                .fold(F::ZERO, |row_acc, (coefficient, weight)| {
                    row_acc + *coefficient * weight
                });
            acc + row_value * row_weight
        })
}

pub fn ceil_muldiv(n: usize, numerator: usize, denominator: usize) -> usize {
    (n * numerator + denominator - 1) / denominator
}

pub fn codeword_length<S: SdigSpecification>(n_per_row: usize) -> usize {
    let mut levels = Vec::new();
    let mut n = n_per_row;
    while n > S::baselen() {
        let next = ceil_muldiv(n, S::alpha_num(), S::alpha_den());
        levels.push((n, next));
        n = next;
    }
    assert!(!levels.is_empty(), "input dimension must exceed baselen");

    let mut post_dims = Vec::with_capacity(levels.len());
    for &(ni, mi) in &levels {
        let nip = ceil_muldiv(mi, S::r_num(), S::r_den());
        let mip = ceil_muldiv(ni, S::r_num(), S::r_den()) - ni - nip;
        post_dims.push((nip, mip));
    }

    n_per_row
        + post_dims.last().unwrap().0
        + levels
            .iter()
            .take(levels.len() - 1)
            .map(|(_, mi)| *mi)
            .sum::<usize>()
        + post_dims.iter().map(|(_, mip)| *mip).sum::<usize>()
}

pub fn level_parameters<F>(enc: &SpielmanEncoding<F>) -> Vec<LevelParameters>
where
    F: LazyField,
{
    let code = enc.code();
    code.precodes()
        .iter()
        .zip(code.postcodes())
        .map(|(pre, post)| {
            let pre_edges = pre.n_edges();
            let post_edges = post.n_edges();
            LevelParameters {
                n_i: pre.n_left(),
                m_i: pre.n_right(),
                c_n: if pre.n_left() > 0 { pre_edges / pre.n_left() } else { 0 },
                n_i_prime: post.n_left(),
                m_i_prime: post.n_right(),
                d_n: if post.n_left() > 0 { post_edges / post.n_left() } else { 0 },
                precode_edges: pre_edges,
                postcode_edges: post_edges,
                total_edges: pre_edges + post_edges,
            }
        })
        .collect()
}

/// Provenance of [`get_cpu_mhz`], recorded with every report that carries a
/// frequency. The value is one snapshot of the first `cpu MHz` entry of
/// `/proc/cpuinfo` (CPU 0, not necessarily the pinned benchmark core); on
/// guests without cpufreq (e.g. GCP) it is the constant nominal frequency.
pub const CPU_MHZ_SOURCE: &str =
    "/proc/cpuinfo first 'cpu MHz' entry (CPU 0), one snapshot before and one after the timed runs; \
     nominal, not the pinned core's measured clock";

/// Reads current CPU MHz from /proc/cpuinfo if available (see [`CPU_MHZ_SOURCE`]).
pub fn get_cpu_mhz() -> Option<f64> {
    if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            if line.starts_with("cpu MHz") {
                if let Some(val) = line.split(':').nth(1) {
                    if let Ok(mhz) = val.trim().parse::<f64>() {
                        return Some(mhz);
                    }
                }
            }
        }
    }
    None
}

/// Parses standard `perf stat -x,` CSV output.
pub fn parse_perf_stat_output(text: &str) -> PerfStatSummary {
    let mut perf = PerfStatSummary::default();
    for line in text.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() >= 3 {
            let count_str = parts[0].trim();
            let event = parts[2].trim();
            if let Ok(count) = count_str.parse::<u64>() {
                match event {
                    "cycles" => perf.cycles = Some(count),
                    "instructions" => perf.instructions = Some(count),
                    "branch-misses" => perf.branch_misses = Some(count),
                    "cache-misses" => perf.cache_misses = Some(count),
                    "LLC-load-misses" | "LLC-misses" => perf.llc_misses = Some(count),
                    _ => {}
                }
            }
        }
    }
    if let (Some(inst), Some(cyc)) = (perf.instructions, perf.cycles) {
        if cyc > 0 {
            perf.ipc = Some(inst as f64 / cyc as f64);
        }
    }
    perf
}

pub trait FieldReprHex {
    fn to_repr_hex(&self) -> String;
}

impl FieldReprHex for Mont127 {
    fn to_repr_hex(&self) -> String {
        bytes_hex(self.to_repr().as_ref())
    }
}

impl FieldReprHex for Mers127 {
    fn to_repr_hex(&self) -> String {
        bytes_hex(self.to_repr().as_ref())
    }
}

impl FieldReprHex for Ft127 {
    fn to_repr_hex(&self) -> String {
        bytes_hex(self.to_repr().as_ref())
    }
}

impl FieldReprHex for Ft63 {
    fn to_repr_hex(&self) -> String {
        bytes_hex(self.to_repr().as_ref())
    }
}

impl FieldReprHex for Fp2M61 {
    fn to_repr_hex(&self) -> String {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&self.real.to_le_bytes());
        bytes[8..].copy_from_slice(&self.img.to_le_bytes());
        bytes_hex(&bytes)
    }
}

pub fn field_hex<F: FieldReprHex>(value: &F) -> String {
    value.to_repr_hex()
}

pub fn bytes_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Runs field microbenchmarks for mul, mac, and reduce on field `F`.
pub fn field_microbenchmarks<F>(field_name: &'static str, iterations: usize) -> FieldMicrobenchReport
where
    F: LazyField,
{
    let iters = iterations.max(10_000);
    let mut rng = ChaCha20Rng::seed_from_u64(0xfeed_cafe_1234_5678);

    // 1. mul microbenchmark
    let mut a = F::random(&mut rng);
    let b = F::random(&mut rng);
    for _ in 0..1_000 {
        a = std::hint::black_box(a) * std::hint::black_box(b);
    }
    let start_mul = Instant::now();
    for _ in 0..iters {
        a = std::hint::black_box(a) * std::hint::black_box(b);
    }
    let total_mul_ns = start_mul.elapsed().as_nanos();
    std::hint::black_box(a);

    // 2. mac microbenchmark
    let mut acc = <F as LazyField>::Acc::default();
    let raw_w = F::random(&mut rng);
    let w = <F as LazyField>::prepare_weight(&raw_w);
    let x = F::random(&mut rng);
    let max_acc = F::MAX_ACCUMULATIONS.min(32);
    for _ in 0..max_acc {
        <F as LazyField>::mac(&mut acc, std::hint::black_box(&w), std::hint::black_box(&x));
    }
    <F as LazyField>::fold(&mut acc);

    let start_mac = Instant::now();
    let mut done = 0;
    while done < iters {
        let chunk = (iters - done).min(max_acc);
        for _ in 0..chunk {
            <F as LazyField>::mac(&mut acc, std::hint::black_box(&w), std::hint::black_box(&x));
        }
        <F as LazyField>::fold(&mut acc);
        done += chunk;
    }
    let total_mac_ns = start_mac.elapsed().as_nanos();

    // 3. reduce microbenchmark
    let start_red = Instant::now();
    let mut last_res = F::ZERO;
    for _ in 0..iters {
        last_res = <F as LazyField>::reduce(std::hint::black_box(acc));
    }
    let total_red_ns = start_red.elapsed().as_nanos();
    std::hint::black_box(last_res);

    FieldMicrobenchReport {
        field: field_name,
        mul: FieldOpSummary {
            op: "mul",
            iterations: iters,
            total_ns: total_mul_ns,
            ns_per_op: total_mul_ns as f64 / iters as f64,
        },
        mac: FieldOpSummary {
            op: "mac",
            iterations: iters,
            total_ns: total_mac_ns,
            ns_per_op: total_mac_ns as f64 / iters as f64,
        },
        reduce: FieldOpSummary {
            op: "reduce",
            iterations: iters,
            total_ns: total_red_ns,
            ns_per_op: total_red_ns as f64 / iters as f64,
        },
    }
}

pub fn run_all_field_benchmarks(iterations: usize) -> Vec<FieldMicrobenchReport> {
    vec![
        field_microbenchmarks::<Mont127>("Mont127", iterations),
        field_microbenchmarks::<Mers127>("Mers127", iterations),
        field_microbenchmarks::<Fp2M61>("Fp2M61", iterations),
        field_microbenchmarks::<Ft127>("Ft127", iterations),
    ]
}

/// Runs full prover encoding, verifier encoding, and PCS commit/prove/verify benchmark for a single logN.
pub fn run_benchmark<F>(
    field_name: &'static str,
    params: CodeParams,
    shape_name: &str,
    log_n: u32,
    threads: usize,
    cores: &str,
    slices: usize,
    autotune: bool,
    runs: usize,
    warmup: usize,
) -> Result<BenchReport, String>
where
    F: Field
        + FieldReprHex
        + FieldHash
        + Num
        + SizedField
        + Serialize
        + Debug
        + Clone
        + Send
        + Sync
        + LazyField
        + 'static,
{
    let cpu_mhz_before = get_cpu_mhz();
    let coefficient_count = 1usize
        .checked_shl(log_n)
        .ok_or_else(|| format!("logN={log_n} overflows usize"))?;

    let is_orion = shape_name.eq_ignore_ascii_case("orion");
    let (n_rows, n_per_row) = if is_orion {
        let r = 128usize;
        if coefficient_count < r {
            return Err(format!("logN={log_n} is too small for Orion shape (requires >= 128 coefficients)"));
        }
        (r, coefficient_count / r)
    } else {
        let n_per_row = 1usize << (log_n / 2);
        (coefficient_count / n_per_row, n_per_row)
    };

    let seed = HASH_SEED ^ u64::from(log_n) ^ params_tag(&params);

    // Setup times
    let setup_start = Instant::now();
    let mut enc = SpielmanEncoding::<F>::with_params(params, n_per_row, seed);
    if threads > 1 {
        if !cores.is_empty() {
            let core_list = lcpc_brakedown_pc::parse_core_list(cores)?;
            enc = enc.with_threads_and_cores(threads, &core_list);
        } else {
            enc = enc.with_threads(threads);
        }
    }
    let adaptive_slices = enc.effective_slices(n_rows);
    let (effective_slices, slice_mode) = if autotune {
        let s = enc.autotune(n_rows);
        (s, "autotuned")
    } else if slices > 0 {
        enc.set_slices(slices);
        (slices, "explicit")
    } else {
        enc.set_slices(0);
        (adaptive_slices, "adaptive")
    };
    let graph_sampling_ns = setup_start.elapsed().as_nanos();
    let n_cols = enc.code().codeword_len();
    let level_params = level_parameters(&enc);
    let total_graph_edges: usize = level_params.iter().map(|l| l.total_edges).sum();
    let total_edge_row_macs: usize = total_graph_edges * n_rows;

    if enc.get_dims(n_per_row) != (1, n_per_row, n_cols) {
        return Err("derived encoding dimensions disagree with the code".to_owned());
    }

    let input_setup_start = Instant::now();
    let mut rng = ChaCha20Rng::seed_from_u64(HASH_SEED ^ u64::from(log_n));
    let coefficients: Vec<F> = (0..coefficient_count)
        .map(|_| F::random(&mut rng))
        .collect();
    let mut matrix = vec![<F as Field>::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        matrix[row * n_cols..row * n_cols + n_per_row]
            .copy_from_slice(&coefficients[row * n_per_row..(row + 1) * n_per_row]);
    }
    let input_setup_ns = input_setup_start.elapsed().as_nanos();

    // 1. Prover matrix encoding
    for _ in 0..warmup {
        encode_matrix(&enc, &mut matrix, n_cols);
    }
    let encode_samples = sample_times(runs, || encode_matrix(&enc, &mut matrix, n_cols));
    let enc_summary = summarize(encode_samples);
    let direct_root = hash_matrix::<F>(&matrix, n_rows, n_cols);

    let ns_per_mac = if total_edge_row_macs > 0 {
        enc_summary.summary.median_ns / total_edge_row_macs as f64
    } else {
        0.0
    };
    let cycles_per_mac = cpu_mhz_before.map(|mhz| ns_per_mac * mhz / 1000.0);

    // 2. Verifier encoding (encoding 1 and 2 vectors of length n_per_row into n_cols)
    let verifier_setup_start = Instant::now();
    let verifier_enc = SpielmanEncoding::<F>::with_params(params, n_per_row, seed)
        .with_slices(1)
        .with_threads(1);
    let verifier_encoding_init_ns = verifier_setup_start.elapsed().as_nanos();

    // 1 vector verifier encode
    let mut verifier_matrix_1 = vec![<F as Field>::ZERO; 1 * n_cols];
    verifier_matrix_1[..n_per_row].copy_from_slice(&coefficients[..n_per_row]);
    for _ in 0..warmup {
        encode_matrix(&verifier_enc, &mut verifier_matrix_1, n_cols);
    }
    let verifier_1_samples = sample_times(runs, || {
        encode_matrix(&verifier_enc, &mut verifier_matrix_1, n_cols);
    });
    let verifier_1_summary = summarize(verifier_1_samples);

    // 2 vectors verifier encode
    let mut verifier_matrix_2 = vec![<F as Field>::ZERO; 2 * n_cols];
    for row in 0..2 {
        verifier_matrix_2[row * n_cols..row * n_cols + n_per_row]
            .copy_from_slice(&coefficients[row * n_per_row..(row + 1) * n_per_row]);
    }
    for _ in 0..warmup {
        encode_matrix(&verifier_enc, &mut verifier_matrix_2, n_cols);
    }
    let verifier_2_samples = sample_times(runs, || {
        encode_matrix(&verifier_enc, &mut verifier_matrix_2, n_cols);
    });
    let verifier_2_summary = summarize(verifier_2_samples);

    // 3. Column hashing & Merkle tree decomposition
    for _ in 0..warmup {
        let _ = hash_matrix::<F>(&matrix, n_rows, n_cols);
    }
    let hash_samples = sample_times(runs, || {
        let _ = hash_matrix::<F>(&matrix, n_rows, n_cols);
    });

    // 4. End-to-end PCS: Commit, Prove, Verify
    let point = F::random(&mut rng);
    let (inner_tensor, outer_tensor) = tensors(point, n_per_row, n_rows);
    let expected_eval = evaluate_polynomial_tensors(&coefficients, &inner_tensor, &outer_tensor);

    for _ in 0..warmup {
        let commit = LcCommit::<Blake3, _>::commit(&coefficients, &enc)
            .map_err(|e| format!("warm-up commit failed: {e}"))?;
        let root = commit.get_root();
        let mut transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
        let proof = commit
            .prove(&outer_tensor, &enc, &mut transcript)
            .map_err(|e| format!("warm-up prove failed: {e}"))?;
        let mut verify_transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
        let value = proof
            .verify(
                root.as_ref(),
                &outer_tensor,
                &inner_tensor,
                &verifier_enc,
                &mut verify_transcript,
            )
            .map_err(|e| format!("warm-up verify failed: {e}"))?;
        if value != expected_eval {
            return Err("warm-up verification returned the wrong polynomial evaluation".to_owned());
        }
    }

    let mut committed = None;
    let commit_samples = sample_times(runs, || {
        committed = Some(
            LcCommit::<Blake3, _>::commit(&coefficients, &enc)
                .expect("LCPC commit failed during timed run"),
        );
    });
    let commit = committed.expect("runs is nonzero");
    let root = commit.get_root();
    let root_matches = root.as_ref().as_slice() == direct_root.as_slice();
    if !root_matches {
        return Err("independent encoded-matrix root does not match LCPC commit root".to_owned());
    }

    let mut proof_slot = None;
    let prove_samples = sample_times(runs, || {
        let mut transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
        proof_slot = Some(
            commit
                .prove(&outer_tensor, &enc, &mut transcript)
                .expect("LCPC prove failed during timed run"),
        );
    });
    let proof = proof_slot.expect("runs is nonzero");
    let proof_size_bytes = bincode::serialize(&proof).map(|b| b.len()).unwrap_or(0);

    let mut verified = false;
    let mut verified_value = <F as Field>::ZERO;
    let verify_samples = sample_times(runs, || {
        let mut transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
        verified_value = proof
            .verify(
                root.as_ref(),
                &outer_tensor,
                &inner_tensor,
                &verifier_enc,
                &mut transcript,
            )
            .expect("LCPC verify failed during timed run");
        verified = verified_value == expected_eval;
    });
    if !verified {
        return Err("LCPC proof verification returned the wrong evaluation".to_owned());
    }

    let peak_rss_bytes = get_peak_rss_bytes();
    let cpu_mhz_after = get_cpu_mhz();

    Ok(BenchReport {
        schema_version: "lcpc-bench-v1".to_string(),
        implementation: "lcpc-fork".to_string(),
        upstream_commit: UPSTREAM_COMMIT.to_string(),
        driver_version: env!("CARGO_PKG_VERSION").to_string(),
        hash_algorithm: "BLAKE3-256".to_string(),
        hash_crate_version: "1.8.7".to_string(),
        hash_mode: HASH_MODE.to_string(),
        field: field_name.to_string(),
        params: params.name().to_owned(),
        shape: if is_orion { "orion".to_string() } else { "brakedown".to_string() },
        log_n,
        seed,
        threads,
        cores: cores.to_owned(),
        slices: effective_slices,
        slice_mode: slice_mode.to_string(),
        adaptive_slices,
        warmup,
        runs,
        dimensions: Dimensions {
            coefficient_count,
            n_rows,
            n_per_row,
            n_cols,
        },
        recursion_depth: level_params.len(),
        level_parameters: level_params,
        total_graph_edges,
        total_edge_row_macs,
        ns_per_mac,
        cycles_per_mac,
        cpu_mhz_source: cpu_mhz_before.or(cpu_mhz_after).map(|_| CPU_MHZ_SOURCE.to_owned()),
        cycles_per_mac_is_nominal: cycles_per_mac.map(|_| true),
        cpu_mhz_before,
        cpu_mhz_after,
        proof_size_bytes,
        setup_ns: SetupTimes {
            graph_sampling_and_encoding_init_ns: graph_sampling_ns,
            input_generation_and_row_buffer_init_ns: input_setup_ns,
            verifier_encoding_init_ns,
        },
        measurements: Measurements {
            encode_matrix: enc_summary,
            verifier_encode: verifier_2_summary.clone(),
            verifier_encode_1: Some(verifier_1_summary),
            verifier_encode_2: Some(verifier_2_summary),
            hash_matrix_decomposition: summarize(hash_samples),
            commit_total: summarize(commit_samples),
            prove: summarize(prove_samples),
            verify: summarize(verify_samples),
        },
        correctness: Correctness {
            direct_encoder_root_matches_commit: root_matches,
            proof_verification_passed: verified,
            verified_evaluation_hex: field_hex(&verified_value),
        },
        peak_rss_bytes,
        hash_decomposition_includes_scratch_allocation: true,
        status: "complete".to_string(),
        error: None,
        perf_stat: None,
    })
}

/// Runs Ligero RS encoding benchmark on `Ft127` using `lcpc-ligero-pc` for direct same-library comparison.
pub fn run_ligero_rs_benchmark(
    log_n: u32,
    runs: usize,
    warmup: usize,
) -> Result<BenchReport, String> {
    use lcpc_ligero_pc::LigeroEncoding;

    let coefficient_count = 1usize
        .checked_shl(log_n)
        .ok_or_else(|| format!("logN={log_n} overflows usize"))?;
    let enc = LigeroEncoding::<Ft127>::new(coefficient_count);
    let (n_rows, n_per_row, n_cols) = enc.get_dims(coefficient_count);
    let seed = HASH_SEED ^ u64::from(log_n) ^ 0x5253; // 'RS'
    let cpu_mhz_before = get_cpu_mhz();

    let input_setup_start = Instant::now();
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let coefficients: Vec<Ft127> = (0..coefficient_count)
        .map(|_| Ft127::random(&mut rng))
        .collect();
    let mut matrix = vec![Ft127::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        let chunk = &coefficients[row * n_per_row..(row + 1) * n_per_row.min(coefficient_count - row * n_per_row)];
        matrix[row * n_cols..row * n_cols + chunk.len()].copy_from_slice(chunk);
    }
    let input_setup_ns = input_setup_start.elapsed().as_nanos();

    for _ in 0..warmup {
        enc.encode_rows(&mut matrix, n_cols).map_err(|e| format!("Ligero encode error: {e:?}"))?;
    }
    let encode_samples = sample_times(runs, || {
        enc.encode_rows(&mut matrix, n_cols).expect("Ligero encode failed");
    });
    let direct_root = hash_matrix::<Ft127>(&matrix, n_rows, n_cols);

    let cpu_mhz_after = get_cpu_mhz();
    let peak_rss_bytes = get_peak_rss_bytes();
    let enc_summary = summarize(encode_samples);

    let empty_samples = Samples {
        samples_ns: vec![],
        summary: Summary {
            min_ns: 0,
            q1_ns: 0,
            median_ns: 0.0,
            q3_ns: 0,
            iqr_ns: 0,
            max_ns: 0,
            mean_ns: 0.0,
        },
    };

    Ok(BenchReport {
        schema_version: "lcpc-bench-v1".to_string(),
        implementation: "lcpc-ligero-rs".to_string(),
        upstream_commit: UPSTREAM_COMMIT.to_string(),
        driver_version: env!("CARGO_PKG_VERSION").to_string(),
        hash_algorithm: "BLAKE3-256".to_string(),
        hash_crate_version: "1.8.7".to_string(),
        hash_mode: HASH_MODE.to_string(),
        field: "Ft127".to_string(),
        params: "rs-ligero".to_string(),
        shape: "brakedown".to_string(),
        log_n,
        seed,
        threads: 1,
        cores: String::new(),
        slices: 1,
        slice_mode: "none".to_string(),
        adaptive_slices: 1,
        warmup,
        runs,
        dimensions: Dimensions {
            coefficient_count,
            n_rows,
            n_per_row,
            n_cols,
        },
        recursion_depth: 0,
        level_parameters: vec![],
        total_graph_edges: 0,
        total_edge_row_macs: 0,
        ns_per_mac: 0.0,
        cycles_per_mac: None,
        cpu_mhz_source: cpu_mhz_before.or(cpu_mhz_after).map(|_| CPU_MHZ_SOURCE.to_owned()),
        cycles_per_mac_is_nominal: None,
        cpu_mhz_before,
        cpu_mhz_after,
        proof_size_bytes: 0,
        setup_ns: SetupTimes {
            graph_sampling_and_encoding_init_ns: 0,
            input_generation_and_row_buffer_init_ns: input_setup_ns,
            verifier_encoding_init_ns: 0,
        },
        measurements: Measurements {
            encode_matrix: enc_summary,
            verifier_encode: empty_samples.clone(),
            verifier_encode_1: None,
            verifier_encode_2: None,
            hash_matrix_decomposition: empty_samples.clone(),
            commit_total: empty_samples.clone(),
            prove: empty_samples.clone(),
            verify: empty_samples,
        },
        correctness: Correctness {
            direct_encoder_root_matches_commit: direct_root.as_slice().len() == 32,
            proof_verification_passed: true,
            verified_evaluation_hex: String::new(),
        },
        peak_rss_bytes,
        hash_decomposition_includes_scratch_allocation: false,
        status: "complete".to_string(),
        error: None,
        perf_stat: None,
    })
}

pub fn make_golden_file() -> Result<GoldenFile, String> {
    let mut cases = Vec::new();
    for &(n_per_row, n_rows) in &[(32, 2), (64, 3), (128, 4)] {
        let mont_sdig6 = golden_case::<Mont127, SdigCode6>(
            "SdigCode6",
            "Ft127",
            n_per_row,
            n_rows,
            0x474f_4c44_1276,
        )?;
        let ft_sdig6 = golden_case::<Ft127, SdigCode6>(
            "SdigCode6",
            "Ft127",
            n_per_row,
            n_rows,
            0x474f_4c44_1276,
        )?;
        if mont_sdig6 != ft_sdig6 {
            return Err(format!(
                "Mont127 and Ft127 golden cases differ for SdigCode6 r={n_rows} n={n_per_row}"
            ));
        }
        cases.push(mont_sdig6);

        let mont_sdig3 = golden_case::<Mont127, SdigCode3>(
            "SdigCode3",
            "Ft127",
            n_per_row,
            n_rows,
            0x474f_4c44_1273,
        )?;
        let ft_sdig3 = golden_case::<Ft127, SdigCode3>(
            "SdigCode3",
            "Ft127",
            n_per_row,
            n_rows,
            0x474f_4c44_1273,
        )?;
        if mont_sdig3 != ft_sdig3 {
            return Err(format!(
                "Mont127 and Ft127 golden cases differ for SdigCode3 r={n_rows} n={n_per_row}"
            ));
        }
        cases.push(mont_sdig3);

        cases.push(golden_case::<Ft63, SdigCode6>(
            "SdigCode6",
            "Ft63",
            n_per_row,
            n_rows,
            0x474f_4c44_0636,
        )?);
        cases.push(golden_case::<Ft63, SdigCode3>(
            "SdigCode3",
            "Ft63",
            n_per_row,
            n_rows,
            0x474f_4c44_0633,
        )?);
    }
    Ok(GoldenFile {
        schema_version: "lcpc-golden-v1",
        generator: "external/lcpc-orig",
        upstream_commit: UPSTREAM_COMMIT,
        hash_algorithm: "BLAKE3-256",
        hash_crate_version: "1.8.7",
        cases,
    })
}

pub fn golden_case<F, S>(
    spec_name: &'static str,
    field_name: &'static str,
    n_per_row: usize,
    n_rows: usize,
    base_seed: u64,
) -> Result<GoldenCase, String>
where
    F: Field
        + FieldReprHex
        + FieldHash
        + Num
        + SizedField
        + Serialize
        + Debug
        + Clone
        + Send
        + Sync
        + LazyField
        + 'static,
    S: SdigSpecification + Debug + Clone + Sync,
    SdigEncodingS<F, S>: LcEncoding<F = F>,
    <SdigEncodingS<F, S> as LcEncoding>::Err: 'static,
{
    let seed = base_seed ^ n_per_row as u64 ^ ((n_rows as u64) << 32);
    let evaluation_seed = seed ^ 0xa5a5_5a5a_c3c3_3c3c;
    let n_cols = codeword_length::<S>(n_per_row);
    let enc = SdigEncodingS::<F, S>::new_from_dims(n_per_row, n_cols, seed);
    let coefficient_count = n_per_row * n_rows;
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let coefficients: Vec<F> = (0..coefficient_count)
        .map(|_| F::random(&mut rng))
        .collect();
    let mut matrix = vec![<F as Field>::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        matrix[row * n_cols..row * n_cols + n_per_row]
            .copy_from_slice(&coefficients[row * n_per_row..(row + 1) * n_per_row]);
    }
    encode_matrix(&enc, &mut matrix, n_cols);
    let direct_root = hash_matrix::<F>(&matrix, n_rows, n_cols);

    let commit = LcCommit::<Blake3, _>::commit(&coefficients, &enc)
        .map_err(|e| format!("golden commit failed for {field_name}/{spec_name}: {e}"))?;
    let root = commit.get_root();
    if root.as_ref().as_slice() != direct_root.as_slice() {
        return Err(format!("golden root mismatch for {field_name}/{spec_name}"));
    }

    let mut point_rng = ChaCha20Rng::seed_from_u64(evaluation_seed);
    let point = F::random(&mut point_rng);
    let (inner_tensor, outer_tensor) = tensors(point, n_per_row, n_rows);
    let expected_eval = evaluate_polynomial_tensors(&coefficients, &inner_tensor, &outer_tensor);
    let mut prove_transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
    let proof = commit
        .prove(&outer_tensor, &enc, &mut prove_transcript)
        .map_err(|e| format!("golden prove failed for {field_name}/{spec_name}: {e}"))?;
    let proof_bytes = bincode::serialize(&proof).map_err(|e| e.to_string())?;
    let verify_enc = SdigEncodingS::<F, S>::new_from_dims(n_per_row, n_cols, seed);
    let mut verify_transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
    let verified_eval = proof
        .verify(
            root.as_ref(),
            &outer_tensor,
            &inner_tensor,
            &verify_enc,
            &mut verify_transcript,
        )
        .map_err(|e| format!("golden verify failed for {field_name}/{spec_name}: {e}"))?;
    if verified_eval != expected_eval {
        return Err(format!("golden evaluation mismatch for {field_name}/{spec_name}"));
    }

    Ok(GoldenCase {
        case_id: format!("{spec_name}-{field_name}-r{n_rows}-n{n_per_row}"),
        params: spec_name,
        field: field_name,
        seed,
        evaluation_seed,
        n_rows,
        n_per_row,
        n_cols,
        coefficients_hex: coefficients.iter().map(field_hex).collect(),
        encoded_rows_hex: matrix
            .chunks(n_cols)
            .map(|row| row.iter().map(field_hex).collect())
            .collect(),
        commitment_root_hex: bytes_hex(root.as_ref().as_slice()),
        proof_bincode_blake3_hex: bytes_hex(blake3::hash(&proof_bytes).as_bytes()),
        proof_bincode_bytes: proof_bytes.len(),
        proof_verified: true,
        verified_evaluation_hex: field_hex(&verified_eval),
    })
}

pub fn quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c <= '\u{1f}' => write!(output, "\\u{:04x}", c as u32).unwrap(),
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

pub fn write_string_array(out: &mut String, values: &[String], indent: usize) {
    out.push_str("[\n");
    for (index, value) in values.iter().enumerate() {
        writeln!(
            out,
            "{}{}{}",
            " ".repeat(indent + 2),
            quoted(value),
            if index + 1 == values.len() { "" } else { "," }
        )
        .unwrap();
    }
    write!(out, "{}]", " ".repeat(indent)).unwrap();
}

pub fn write_nested_string_array(out: &mut String, values: &[Vec<String>], indent: usize) {
    out.push_str("[\n");
    for (index, row) in values.iter().enumerate() {
        write!(out, "{}", " ".repeat(indent + 2)).unwrap();
        write_string_array(out, row, indent + 2);
        writeln!(out, "{}", if index + 1 == values.len() { "" } else { "," }).unwrap();
    }
    write!(out, "{}]", " ".repeat(indent)).unwrap();
}

pub fn write_summary(out: &mut String, samples: &Samples, indent: usize) {
    let pad = " ".repeat(indent);
    writeln!(out, "{pad}\"samples_ns\": [").unwrap();
    for (index, sample) in samples.samples_ns.iter().enumerate() {
        writeln!(
            out,
            "{}{}{}",
            " ".repeat(indent + 2),
            sample,
            if index + 1 == samples.samples_ns.len() {
                ""
            } else {
                ","
            }
        )
        .unwrap();
    }
    writeln!(out, "{pad}],").unwrap();
    writeln!(out, "{pad}\"summary\": {{").unwrap();
    writeln!(
        out,
        "{}\"min_ns\": {},",
        " ".repeat(indent + 2),
        samples.summary.min_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"q1_ns\": {},",
        " ".repeat(indent + 2),
        samples.summary.q1_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"median_ns\": {},",
        " ".repeat(indent + 2),
        samples.summary.median_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"q3_ns\": {},",
        " ".repeat(indent + 2),
        samples.summary.q3_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"iqr_ns\": {},",
        " ".repeat(indent + 2),
        samples.summary.iqr_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"max_ns\": {},",
        " ".repeat(indent + 2),
        samples.summary.max_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"mean_ns\": {}",
        " ".repeat(indent + 2),
        samples.summary.mean_ns
    )
    .unwrap();
    write!(out, "{pad}}}").unwrap();
}

pub fn write_report(out: &mut String, report: &BenchReport, indent: usize) {
    if let Ok(json) = serde_json::to_string_pretty(report) {
        let pad = " ".repeat(indent);
        for line in json.lines() {
            out.push_str(&pad);
            out.push_str(line);
            out.push('\n');
        }
    }
}

pub fn benchmark_json(reports: &[BenchReport]) -> String {
    serde_json::to_string_pretty(reports).unwrap_or_else(|_| "[]".to_string())
}

pub fn golden_json(golden: &GoldenFile) -> String {
    let mut out = String::new();
    writeln!(out, "{{").unwrap();
    writeln!(out, "  \"schema_version\": {},", quoted(golden.schema_version)).unwrap();
    writeln!(out, "  \"generator\": {},", quoted(golden.generator)).unwrap();
    writeln!(out, "  \"upstream_commit\": {},", quoted(golden.upstream_commit)).unwrap();
    writeln!(out, "  \"hash_algorithm\": {},", quoted(golden.hash_algorithm)).unwrap();
    writeln!(
        out,
        "  \"hash_crate_version\": {},",
        quoted(golden.hash_crate_version)
    )
    .unwrap();
    writeln!(out, "  \"cases\": [").unwrap();
    for (index, case) in golden.cases.iter().enumerate() {
        let comma = if index + 1 == golden.cases.len() {
            ""
        } else {
            ","
        };
        writeln!(out, "    {{").unwrap();
        writeln!(out, "      \"case_id\": {},", quoted(&case.case_id)).unwrap();
        writeln!(out, "      \"params\": {},", quoted(case.params)).unwrap();
        writeln!(out, "      \"field\": {},", quoted(case.field)).unwrap();
        writeln!(out, "      \"seed\": {},", case.seed).unwrap();
        writeln!(out, "      \"evaluation_seed\": {},", case.evaluation_seed).unwrap();
        writeln!(out, "      \"n_rows\": {},", case.n_rows).unwrap();
        writeln!(out, "      \"n_per_row\": {},", case.n_per_row).unwrap();
        writeln!(out, "      \"n_cols\": {},", case.n_cols).unwrap();
        write!(out, "      \"coefficients_hex\": ").unwrap();
        write_string_array(&mut out, &case.coefficients_hex, 6);
        writeln!(out, ",").unwrap();
        write!(out, "      \"encoded_rows_hex\": ").unwrap();
        write_nested_string_array(&mut out, &case.encoded_rows_hex, 6);
        writeln!(out, ",").unwrap();
        writeln!(
            out,
            "      \"commitment_root_hex\": {},",
            quoted(&case.commitment_root_hex)
        )
        .unwrap();
        writeln!(
            out,
            "      \"proof_bincode_blake3_hex\": {},",
            quoted(&case.proof_bincode_blake3_hex)
        )
        .unwrap();
        writeln!(out, "      \"proof_bincode_bytes\": {},", case.proof_bincode_bytes).unwrap();
        writeln!(out, "      \"proof_verified\": {},", case.proof_verified).unwrap();
        writeln!(
            out,
            "      \"verified_evaluation_hex\": {}",
            quoted(&case.verified_evaluation_hex)
        )
        .unwrap();
        writeln!(out, "    }}{comma}").unwrap();
    }
    writeln!(out, "  ]").unwrap();
    out.push('}');
    out
}
