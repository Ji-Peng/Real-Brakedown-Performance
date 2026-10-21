// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Thin benchmark driver for arkworks' Brakedown linear-code encoder.
//!
//! The timed region encodes every row of the coefficient matrix. Graph setup,
//! deterministic input generation, and output allocation are outside the timer.

use std::borrow::Borrow;
use std::fs;
use std::marker::PhantomData;
use std::path::Path;
use std::time::Instant;

use ark_crypto_primitives::crh::{CRHScheme, TwoToOneCRHScheme};
use ark_crypto_primitives::merkle_tree::{Config as MerkleConfig, IdentityDigestConverter};
use ark_crypto_primitives::sponge::Absorb;
use ark_ff::{fields::Fp128, MontBackend, MontConfig, PrimeField, UniformRand};
use ark_poly::evaluations::multivariate::multilinear::DenseMultilinearExtension;
use ark_poly_commit::linear_codes::{
    BrakedownPCParams, LinearEncode, MultilinearBrakedown,
};
use ark_std::rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rayon::prelude::*;
use rayon::ThreadPool;
use serde::Serialize;

const POLY_COMMIT_COMMIT: &str = "1eaec5090ba5bc87d75a69570ed38f6fcf8b9417";
const FIELD_MODULUS: &str = "146823888364060453008360742206866194433";
const SEED: u64 = 0x504f_4c59_434f_4d4d;
const DEFAULT_SEC_PARAM: usize = 80;

#[derive(MontConfig)]
#[modulus = "146823888364060453008360742206866194433"]
#[generator = "3"]
pub struct Mont127Config;

type Mont127 = Fp128<MontBackend<Mont127Config, 2>>;

/// A marker CRH used only to satisfy the generic PCS parameter type.
///
/// The benchmark invokes only `LinearEncode::encode`; it never hashes leaves,
/// columns, or Merkle nodes. These methods intentionally panic if that scope is
/// accidentally widened without replacing the marker with real hash configs.
struct EncodingOnlySliceHash<F>(PhantomData<F>);

impl<F: PrimeField + Absorb> CRHScheme for EncodingOnlySliceHash<F> {
    type Input = [F];
    type Output = F;
    type Parameters = ();

    fn setup<R: Rng>(_rng: &mut R) -> Result<Self::Parameters, ark_crypto_primitives::Error> {
        Ok(())
    }

    fn evaluate<T: Borrow<Self::Input>>(
        _parameters: &Self::Parameters,
        _input: T,
    ) -> Result<Self::Output, ark_crypto_primitives::Error> {
        panic!("encoding-only marker hash was invoked")
    }
}

/// A marker two-to-one CRH. See [`EncodingOnlySliceHash`].
struct EncodingOnlyPairHash<F>(PhantomData<F>);

impl<F: PrimeField + Absorb> TwoToOneCRHScheme for EncodingOnlyPairHash<F> {
    type Input = F;
    type Output = F;
    type Parameters = ();

    fn setup<R: Rng>(_rng: &mut R) -> Result<Self::Parameters, ark_crypto_primitives::Error> {
        Ok(())
    }

    fn evaluate<T: Borrow<Self::Input>>(
        _parameters: &Self::Parameters,
        _left_input: T,
        _right_input: T,
    ) -> Result<Self::Output, ark_crypto_primitives::Error> {
        panic!("encoding-only marker hash was invoked")
    }

    fn compress<T: Borrow<Self::Output>>(
        _parameters: &Self::Parameters,
        _left_input: T,
        _right_input: T,
    ) -> Result<Self::Output, ark_crypto_primitives::Error> {
        panic!("encoding-only marker hash was invoked")
    }
}

struct EncodingOnlyMerkleConfig<F>(PhantomData<F>);

impl<F: PrimeField + Absorb> MerkleConfig for EncodingOnlyMerkleConfig<F> {
    type Leaf = [F];
    type LeafDigest = F;
    type LeafInnerDigestConverter = IdentityDigestConverter<F>;
    type InnerDigest = F;
    type LeafHash = EncodingOnlySliceHash<F>;
    type TwoToOneHash = EncodingOnlyPairHash<F>;
}

type Tree = EncodingOnlyMerkleConfig<Mont127>;
type Polynomial = DenseMultilinearExtension<Mont127>;
type ColumnHash = EncodingOnlySliceHash<Mont127>;
type Code = MultilinearBrakedown<Mont127, Tree, Polynomial, ColumnHash>;
type Parameters = BrakedownPCParams<Mont127, Tree, ColumnHash>;

#[derive(Debug)]
struct Args {
    logn: Vec<u32>,
    runs: usize,
    warmup: usize,
    threads: usize,
    cores: String,
    sec_param: usize,
    output: Option<String>,
    check: bool,
}

#[derive(Serialize)]
struct Summary {
    min_ns: u128,
    q1_ns: u128,
    median_ns: f64,
    q3_ns: u128,
    iqr_ns: u128,
    max_ns: u128,
    mean_ns: f64,
}

#[derive(Serialize)]
struct Samples {
    samples_ns: Vec<u128>,
    summary: Summary,
}

#[derive(Serialize)]
struct Dimensions {
    coefficient_count: usize,
    n_rows: usize,
    n_per_row: usize,
    n_cols: usize,
}

#[derive(Serialize)]
struct SetupTimes {
    graph_sampling_and_encoding_init_ns: u128,
    input_generation_and_row_buffer_init_ns: u128,
    verifier_encoding_init_ns: Option<u128>,
}

#[derive(Serialize)]
struct Measurements {
    encode_matrix: Samples,
    verifier_encode: Option<Samples>,
    verifier_encode_1: Option<Samples>,
    verifier_encode_2: Option<Samples>,
    hash_matrix_decomposition: Option<Samples>,
    commit_total: Option<Samples>,
    prove: Option<Samples>,
    verify: Option<Samples>,
}

#[derive(Serialize)]
struct Correctness {
    direct_encoder_root_matches_commit: Option<bool>,
    proof_verification_passed: Option<bool>,
    verified_evaluation_hex: Option<String>,
    encoder_linearity_passed: bool,
}

#[derive(Serialize)]
struct BenchReport {
    schema_version: &'static str,
    implementation: &'static str,
    upstream_commit: &'static str,
    driver_version: &'static str,
    hash_algorithm: &'static str,
    hash_crate_version: Option<&'static str>,
    hash_mode: &'static str,
    field: &'static str,
    field_modulus: &'static str,
    patched: bool,
    patch_sha256: &'static str,
    sec_param: usize,
    params: &'static str,
    shape: &'static str,
    log_n: u32,
    seed: u64,
    threads: usize,
    cores: String,
    slices: Option<usize>,
    slice_mode: &'static str,
    adaptive_slices: Option<usize>,
    warmup: usize,
    runs: usize,
    dimensions: Dimensions,
    recursion_depth: Option<usize>,
    level_parameters: Vec<serde_json::Value>,
    total_graph_edges: Option<usize>,
    total_edge_row_macs: Option<usize>,
    ns_per_mac: Option<f64>,
    cycles_per_mac: Option<f64>,
    cpu_mhz_before: Option<f64>,
    cpu_mhz_after: Option<f64>,
    proof_size_bytes: Option<usize>,
    setup_ns: SetupTimes,
    measurements: Measurements,
    correctness: Correctness,
    peak_rss_bytes: usize,
    hash_decomposition_includes_scratch_allocation: Option<bool>,
    status: &'static str,
    error: Option<String>,
    perf_stat: Option<serde_json::Value>,
    measurement_scope: &'static str,
    hash_functions_invoked: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("polycommit-bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()
        .map_err(|error| format!("cannot create Rayon pool: {error}"))?;

    let mut reports = Vec::with_capacity(args.logn.len());
    for &log_n in &args.logn {
        if args.check {
            check_configuration(log_n, args.sec_param, &pool)?;
            println!("linearity check passed for logN={log_n}");
        } else {
            reports.push(benchmark(&args, log_n, &pool)?);
        }
    }

    if !args.check {
        let json = serde_json::to_string_pretty(&reports)
            .map_err(|error| format!("cannot serialize benchmark output: {error}"))?;
        if let Some(path) = args.output.as_deref() {
            write_output(path, &json)?;
        } else {
            println!("{json}");
        }
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        logn: vec![16],
        runs: 5,
        warmup: 2,
        threads: 1,
        cores: "0".to_owned(),
        sec_param: DEFAULT_SEC_PARAM,
        output: None,
        check: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(argument) = it.next() {
        match argument.as_str() {
            "--logN" => {
                let raw = next_value(&mut it, "--logN")?;
                args.logn = raw
                    .split(',')
                    .map(|part| {
                        part.parse::<u32>()
                            .map_err(|_| format!("invalid --logN value {part:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if args.logn.is_empty()
                    || args.logn.iter().any(|log_n| *log_n < 8 || *log_n % 2 != 0)
                {
                    return Err("--logN values must be even and at least 8".to_owned());
                }
            }
            "--runs" => args.runs = parse_positive(&next_value(&mut it, "--runs")?, "--runs")?,
            "--warmup" => {
                args.warmup = next_value(&mut it, "--warmup")?
                    .parse()
                    .map_err(|_| "invalid --warmup value".to_owned())?;
            }
            "--threads" => {
                args.threads = parse_positive(&next_value(&mut it, "--threads")?, "--threads")?;
            }
            "--cores" => args.cores = next_value(&mut it, "--cores")?,
            "--sec-param" => {
                args.sec_param = parse_positive(&next_value(&mut it, "--sec-param")?, "--sec-param")?;
            }
            "--output" => args.output = Some(next_value(&mut it, "--output")?),
            "--check" => args.check = true,
            "-h" | "--help" => {
                println!(
                    "Usage: polycommit-bench [--logN 16,18] [--runs N] [--warmup N] \
                     [--threads N] [--cores LIST] [--sec-param N] [--output PATH] [--check]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {argument:?}")),
        }
    }
    if args.threads > 1 && args.cores == "0" {
        return Err("provide --cores when --threads > 1".to_owned());
    }
    Ok(args)
}

fn next_value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn parse_positive(value: &str, option: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid {option} value {value:?}"))?;
    if parsed == 0 {
        return Err(format!("{option} must be greater than zero"));
    }
    Ok(parsed)
}

fn benchmark(args: &Args, log_n: u32, pool: &ThreadPool) -> Result<BenchReport, String> {
    let coefficient_count = 1usize
        .checked_shl(log_n)
        .ok_or_else(|| format!("logN={log_n} exceeds this platform's address space"))?;

    let (n_rows, n_per_row) = square_dimensions(coefficient_count, log_n)?;
    let setup_start = Instant::now();
    let mut graph_rng = ChaCha20Rng::seed_from_u64(SEED.wrapping_add(u64::from(log_n)));
    let parameters = Parameters::with_dims(
        &mut graph_rng,
        n_rows,
        n_per_row,
        args.sec_param,
        true,
        (),
        (),
        (),
    );
    let graph_sampling_ns = setup_start.elapsed().as_nanos();
    if n_rows.checked_mul(n_per_row) != Some(coefficient_count) {
        return Err(format!(
            "arkworks returned matrix dimensions {n_rows}x{n_per_row} for {coefficient_count} coefficients"
        ));
    }

    let mut input_generation_ns = 0_u128;
    let mut samples = Vec::with_capacity(args.runs);
    let iterations = args.warmup + args.runs;
    let mut first_output_columns = None;
    let mut linearity_passed = false;

    for iteration in 0..iterations {
        let input_start = Instant::now();
        let input = generate_input(
            coefficient_count,
            SEED.wrapping_add((u64::from(log_n) << 32) | iteration as u64),
        );
        input_generation_ns += input_start.elapsed().as_nanos();

        if iteration == 0 {
            linearity_passed = check_linearity(n_per_row, &parameters, pool)?;
            if !linearity_passed {
                return Err("arkworks linear-code encoder failed the linearity check".to_owned());
            }
        }

        let start = Instant::now();
        let encoded_rows = encode_matrix(&input, n_per_row, &parameters, pool)?;
        let elapsed = start.elapsed().as_nanos();
        if let Some(columns) = first_output_columns {
            if encoded_rows.iter().any(|row| row.len() != columns) {
                return Err("arkworks returned inconsistent encoded row lengths".to_owned());
            }
        } else {
            first_output_columns = encoded_rows.first().map(Vec::len);
        }
        std::hint::black_box(&encoded_rows);

        if iteration >= args.warmup {
            samples.push(elapsed);
        }
    }

    let n_cols = first_output_columns.ok_or_else(|| "encoder returned no rows".to_owned())?;
    let input_generation_ns = input_generation_ns / iterations as u128;
    Ok(BenchReport {
        schema_version: "lcpc-bench-v1",
        implementation: "arkworks-poly-commit-brakedown",
        upstream_commit: POLY_COMMIT_COMMIT,
        driver_version: env!("CARGO_PKG_VERSION"),
        hash_algorithm: "not invoked (encoding-only)",
        hash_crate_version: None,
        hash_mode: "not-invoked",
        field: "Mont127",
        field_modulus: FIELD_MODULUS,
        patched: option_env!("POLYCOMMIT_PATCH_SHA256").is_some(),
        patch_sha256: option_env!("POLYCOMMIT_PATCH_SHA256").unwrap_or("unpatched-direct-build"),
        sec_param: args.sec_param,
        params: "arkworks-brakedown-explicit-square",
        shape: "brakedown",
        log_n: log_n,
        seed: SEED,
        threads: args.threads,
        cores: args.cores.clone(),
        slices: None,
        slice_mode: "not-applicable",
        adaptive_slices: None,
        warmup: args.warmup,
        runs: args.runs,
        dimensions: Dimensions {
            coefficient_count,
            n_rows,
            n_per_row,
            n_cols,
        },
        recursion_depth: None,
        level_parameters: Vec::new(),
        total_graph_edges: None,
        total_edge_row_macs: None,
        ns_per_mac: None,
        cycles_per_mac: None,
        cpu_mhz_before: None,
        cpu_mhz_after: None,
        proof_size_bytes: None,
        setup_ns: SetupTimes {
            graph_sampling_and_encoding_init_ns: graph_sampling_ns,
            input_generation_and_row_buffer_init_ns: input_generation_ns,
            verifier_encoding_init_ns: None,
        },
        measurements: Measurements {
            encode_matrix: summarize(samples),
            verifier_encode: None,
            verifier_encode_1: None,
            verifier_encode_2: None,
            hash_matrix_decomposition: None,
            commit_total: None,
            prove: None,
            verify: None,
        },
        correctness: Correctness {
            direct_encoder_root_matches_commit: None,
            proof_verification_passed: None,
            verified_evaluation_hex: None,
            encoder_linearity_passed: linearity_passed,
        },
        peak_rss_bytes: peak_rss_bytes(),
        hash_decomposition_includes_scratch_allocation: None,
        status: "complete",
        error: None,
        perf_stat: None,
        measurement_scope: "all matrix rows encoded; graph setup and input generation excluded",
        hash_functions_invoked: false,
    })
}

fn check_configuration(log_n: u32, sec_param: usize, pool: &ThreadPool) -> Result<(), String> {
    let coefficient_count = 1usize
        .checked_shl(log_n)
        .ok_or_else(|| format!("logN={log_n} exceeds this platform's address space"))?;
    let (n_rows, n_per_row) = square_dimensions(coefficient_count, log_n)?;
    let mut graph_rng = ChaCha20Rng::seed_from_u64(SEED.wrapping_add(u64::from(log_n)));
    let parameters = Parameters::with_dims(
        &mut graph_rng,
        n_rows,
        n_per_row,
        sec_param,
        true,
        (),
        (),
        (),
    );
    if check_linearity(n_per_row, &parameters, pool)? {
        Ok(())
    } else {
        Err("arkworks linear-code encoder failed the linearity check".to_owned())
    }
}

fn square_dimensions(coefficient_count: usize, log_n: u32) -> Result<(usize, usize), String> {
    if log_n % 2 != 0 {
        return Err(format!("logN={log_n} must be even for the square benchmark shape"));
    }
    let n_rows = 1usize << (log_n / 2);
    let n_cols = coefficient_count / n_rows;
    if n_rows.checked_mul(n_cols) != Some(coefficient_count) {
        return Err(format!("cannot form a square matrix shape for N={coefficient_count}"));
    }
    Ok((n_rows, n_cols))
}

fn generate_input(length: usize, seed: u64) -> Vec<Mont127> {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    (0..length).map(|_| Mont127::rand(&mut rng)).collect()
}

fn encode_matrix(
    input: &[Mont127],
    n_per_row: usize,
    parameters: &Parameters,
    pool: &ThreadPool,
) -> Result<Vec<Vec<Mont127>>, String> {
    pool.install(|| {
        input
            .par_chunks(n_per_row)
            .map(|row| {
                <Code as LinearEncode<Mont127, Tree, Polynomial, ColumnHash>>::encode(
                    row, parameters,
                )
                .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()
    })
}

fn check_linearity(
    n_per_row: usize,
    parameters: &Parameters,
    pool: &ThreadPool,
) -> Result<bool, String> {
    let left = generate_input(n_per_row, SEED ^ 0x1111_1111_1111_1111);
    let right = generate_input(n_per_row, SEED ^ 0x2222_2222_2222_2222);
    let sum: Vec<_> = left.iter().zip(&right).map(|(a, b)| *a + *b).collect();
    let encode_one = |message: &[Mont127]| {
        pool.install(|| {
            <Code as LinearEncode<Mont127, Tree, Polynomial, ColumnHash>>::encode(
                message, parameters,
            )
        })
        .map_err(|error| error.to_string())
    };
    let encoded_left = encode_one(&left)?;
    let encoded_right = encode_one(&right)?;
    let encoded_sum = encode_one(&sum)?;
    if encoded_left.len() != encoded_right.len() || encoded_left.len() != encoded_sum.len() {
        return Ok(false);
    }
    Ok(encoded_left
        .iter()
        .zip(encoded_right.iter())
        .zip(encoded_sum.iter())
        .all(|((a, b), sum)| *a + *b == *sum))
}

fn summarize(samples: Vec<u128>) -> Samples {
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    let median_ns = if sorted.len() % 2 == 0 {
        (sorted[middle - 1] as f64 + sorted[middle] as f64) / 2.0
    } else {
        sorted[middle] as f64
    };
    let q1_ns = sorted[(sorted.len() - 1) / 4];
    let q3_ns = sorted[3 * (sorted.len() - 1) / 4];
    let mean_ns = samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
    Samples {
        samples_ns: samples,
        summary: Summary {
            min_ns: sorted[0],
            q1_ns,
            median_ns,
            q3_ns,
            iqr_ns: q3_ns.saturating_sub(q1_ns),
            max_ns: *sorted.last().expect("non-empty samples"),
            mean_ns,
        },
    }
}

fn peak_rss_bytes() -> usize {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|kilobytes| kilobytes.parse::<usize>().ok())
        })
        .map(|kilobytes| kilobytes * 1024)
        .unwrap_or(0)
}

fn write_output(path: &str, contents: &str) -> Result<(), String> {
    let path = Path::new(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
    }
    fs::write(path, format!("{contents}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}
