// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

use std::fmt::Debug;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as IoWrite;
use std::path::Path;
use std::time::Instant;

use digest::{FixedOutput, Output, Update};
use ff::{Field, PrimeField};
use lcpc_2d::{FieldHash, LcCommit, LcEncoding, SizedField};
use lcpc_brakedown_pc::codespec::{SdigCode3, SdigCode6, SdigSpecification};
use lcpc_brakedown_pc::SdigEncodingS;
// NOTE (step/02-fields part 2): Also import `Fp2M61` from `lcpc_fields` here once implemented.
use lcpc_fields::{Mers127, Mont127};
use lcpc_test_fields::{ft127::Ft127, ft63::Ft63, Blake3};
use merlin::Transcript;
use num_traits::Num;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use rayon::prelude::*;
use serde::Serialize;
use sprs::MulAcc;

const UPSTREAM_COMMIT: &str = "f0e2116001af5a1bbf39481535f837d6a25772e8";
const HASH_SEED: u64 = 0x4c43_5043_4f52_4947;
const HASH_MODE: &str = "portable-only";

#[derive(Debug)]
struct Args {
    field: String,
    params: String,
    logn: Vec<u32>,
    runs: usize,
    warmup: usize,
    threads: usize,
    cores: String,
    output: Option<String>,
    generate_golden: Option<String>,
    check_golden: Option<String>,
}

#[derive(Serialize)]
struct Summary {
    min_ns: u128,
    q1_ns: u128,
    median_ns: f64,
    q3_ns: u128,
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
struct LevelParameters {
    n_i: usize,
    m_i: usize,
    c_n: usize,
    n_i_prime: usize,
    m_i_prime: usize,
    d_n: usize,
}

#[derive(Serialize)]
struct SetupTimes {
    graph_sampling_and_encoding_init_ns: u128,
    input_generation_and_row_buffer_init_ns: u128,
    verifier_encoding_init_ns: u128,
}

#[derive(Serialize)]
struct Measurements {
    encode_matrix: Samples,
    hash_matrix_decomposition: Samples,
    commit_total: Samples,
    prove: Samples,
    verify: Samples,
}

#[derive(Serialize)]
struct Correctness {
    direct_encoder_root_matches_commit: bool,
    proof_verification_passed: bool,
    verified_evaluation_hex: String,
}

#[derive(Serialize)]
struct BenchReport {
    schema_version: &'static str,
    implementation: &'static str,
    upstream_commit: &'static str,
    driver_version: &'static str,
    hash_algorithm: &'static str,
    hash_crate_version: &'static str,
    hash_mode: &'static str,
    field: &'static str,
    params: String,
    log_n: u32,
    seed: u64,
    threads: usize,
    cores: String,
    warmup: usize,
    runs: usize,
    dimensions: Dimensions,
    recursion_depth: usize,
    level_parameters: Vec<LevelParameters>,
    setup_ns: SetupTimes,
    measurements: Measurements,
    correctness: Correctness,
    hash_decomposition_includes_scratch_allocation: bool,
}

#[derive(Serialize)]
struct GoldenFile {
    schema_version: &'static str,
    generator: &'static str,
    upstream_commit: &'static str,
    hash_algorithm: &'static str,
    hash_crate_version: &'static str,
    cases: Vec<GoldenCase>,
}

#[derive(Serialize, PartialEq, Eq)]
struct GoldenCase {
    case_id: String,
    params: &'static str,
    field: &'static str,
    seed: u64,
    evaluation_seed: u64,
    n_rows: usize,
    n_per_row: usize,
    n_cols: usize,
    coefficients_hex: Vec<String>,
    encoded_rows_hex: Vec<Vec<String>>,
    commitment_root_hex: String,
    proof_bincode_blake3_hex: String,
    proof_bincode_bytes: usize,
    proof_verified: bool,
    verified_evaluation_hex: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lcpc-bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // Ensure non-inlined field kernel symbols are retained in the binary for `scripts/check_codegen.py --strict`
    // even before `step/05-lazy-reduce` wires lazy column reduction into the encoder.
    // NOTE (step/02-fields part 2): Also include `mac_column_fp2m61` and `reduce_column_fp2m61` here.
    std::hint::black_box((
        lcpc_fields::mont127::mac_column_mont127 as *const (),
        lcpc_fields::mont127::reduce_column_mont127 as *const (),
        lcpc_fields::mers127::mac_column_mers127 as *const (),
        lcpc_fields::mers127::reduce_column_mers127 as *const (),
    ));

    let args = parse_args()?;
    std::env::set_var("RAYON_NUM_THREADS", args.threads.to_string());

    if let Some(path) = args.generate_golden.as_deref() {
        let golden = make_golden_file()?;
        let json = golden_json(&golden);
        write_output(path, &json)?;
        return Ok(());
    }

    if let Some(path) = args.check_golden.as_deref() {
        let golden = make_golden_file()?;
        let actual = golden_json(&golden);
        let expected_text = fs::read_to_string(path)
            .map_err(|e| format!("cannot read golden file {path}: {e}"))?;
        if actual != expected_text.trim_end() {
            return Err(format!("golden vectors differ from {path}"));
        }
        println!("golden vectors verified: {path}");
        return Ok(());
    }

    let mut reports = Vec::with_capacity(args.logn.len());
    for &log_n in &args.logn {
        let report = match (args.field.as_str(), args.params.as_str()) {
            ("mont127", "lcpc-sdig6") => {
                benchmark::<Mont127, SdigCode6>(&args, log_n, "Mont127", "lcpc-sdig6", 6)?
            }
            ("mont127", "lcpc-sdig3") => {
                benchmark::<Mont127, SdigCode3>(&args, log_n, "Mont127", "lcpc-sdig3", 3)?
            }
            ("mers127", "lcpc-sdig6") => {
                benchmark::<Mers127, SdigCode6>(&args, log_n, "Mers127", "lcpc-sdig6", 6)?
            }
            ("mers127", "lcpc-sdig3") => {
                benchmark::<Mers127, SdigCode3>(&args, log_n, "Mers127", "lcpc-sdig3", 3)?
            }
            // NOTE (step/02-fields part 2): Add ("fp2m61", "lcpc-sdig6") and ("fp2m61", "lcpc-sdig3") arms here.
            ("ft127", "lcpc-sdig6") => {
                benchmark::<Ft127, SdigCode6>(&args, log_n, "Ft127", "lcpc-sdig6", 6)?
            }
            ("ft127", "lcpc-sdig3") => {
                benchmark::<Ft127, SdigCode3>(&args, log_n, "Ft127", "lcpc-sdig3", 3)?
            }
            ("ft63", "lcpc-sdig6") => {
                benchmark::<Ft63, SdigCode6>(&args, log_n, "Ft63", "lcpc-sdig6", 6)?
            }
            ("ft63", "lcpc-sdig3") => {
                benchmark::<Ft63, SdigCode3>(&args, log_n, "Ft63", "lcpc-sdig3", 3)?
            }
            _ => {
                return Err(format!(
                    "unsupported (field, params) combination: ({}, {})",
                    args.field, args.params
                ))
            }
        };
        reports.push(report);
    }
    let json = benchmark_json(&reports);
    if let Some(path) = args.output.as_deref() {
        write_output(path, &json)?;
    } else {
        println!("{json}");
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        field: "mont127".to_owned(),
        params: "lcpc-sdig6".to_owned(),
        logn: vec![16, 18, 20, 22, 24, 26],
        runs: 30,
        warmup: 5,
        threads: 1,
        cores: String::new(),
        output: None,
        generate_golden: None,
        check_golden: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let value = |it: &mut std::iter::Skip<std::env::Args>, name: &str| {
            it.next().ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--field" => {
                args.field = value(&mut it, "--field")?.to_ascii_lowercase();
                // NOTE (step/02-fields part 2): Add "fp2m61" to this match once implemented.
                if !matches!(args.field.as_str(), "mont127" | "mers127" | "ft127" | "ft63") {
                    return Err("--field must be mont127, mers127, ft127, or ft63".to_owned());
                }
            }
            "--params" => {
                args.params = value(&mut it, "--params")?;
                if !matches!(args.params.as_str(), "lcpc-sdig6" | "lcpc-sdig3") {
                    return Err("--params must be lcpc-sdig6 or lcpc-sdig3".to_owned());
                }
            }
            "--logN" => {
                let raw = value(&mut it, "--logN")?;
                args.logn = raw
                    .split(',')
                    .map(|v| v.parse::<u32>().map_err(|_| format!("invalid logN {v}")))
                    .collect::<Result<Vec<_>, _>>()?;
                if args.logn.is_empty() || args.logn.iter().any(|n| *n < 10 || *n % 2 != 0) {
                    return Err("--logN values must be even and at least 10".to_owned());
                }
            }
            "--runs" => args.runs = parse_usize(&value(&mut it, "--runs")?, "--runs")?,
            "--warmup" => {
                args.warmup = parse_usize(&value(&mut it, "--warmup")?, "--warmup")?
            }
            "--threads" => {
                args.threads = parse_usize(&value(&mut it, "--threads")?, "--threads")?
            }
            "--cores" => args.cores = value(&mut it, "--cores")?,
            "--output" => args.output = Some(value(&mut it, "--output")?),
            "--generate-golden" => args.generate_golden = Some(value(&mut it, "--generate-golden")?),
            "--check-golden" => args.check_golden = Some(value(&mut it, "--check-golden")?),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    if args.runs == 0 || args.threads == 0 {
        return Err("--runs and --threads must be positive".to_owned());
    }
    if args.generate_golden.is_some() && args.check_golden.is_some() {
        return Err("choose only one of --generate-golden and --check-golden".to_owned());
    }
    Ok(args)
}

fn print_help() {
    println!(
        "Usage: lcpc-bench [--field mont127|mers127|ft127|ft63] [--params lcpc-sdig6|lcpc-sdig3] \\\n\
         [--logN 16,18,...] [--runs N] [--warmup N] [--threads N] [--cores LIST] [--output PATH] \\\n\
         [--generate-golden PATH | --check-golden PATH]"
    );
}

fn parse_usize(raw: &str, name: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn write_output(path: &str, text: &str) -> Result<(), String> {
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

fn benchmark<F, S>(
    args: &Args,
    log_n: u32,
    field_name: &'static str,
    params_name: &'static str,
    params_tag: u64,
) -> Result<BenchReport, String>
where
    F: Field
        + FieldReprHex
        + FieldHash
        + MulAcc
        + Num
        + SizedField
        + Serialize
        + Debug
        + Clone
        + Send
        + Sync
        + 'static,
    S: SdigSpecification + Debug + Clone + Sync,
    SdigEncodingS<F, S>: LcEncoding<F = F>,
    <SdigEncodingS<F, S> as LcEncoding>::Err: 'static,
{
    let coefficient_count = 1usize
        .checked_shl(log_n)
        .ok_or_else(|| format!("logN={log_n} overflows usize"))?;
    let n_per_row = 1usize << (log_n / 2);
    let n_rows = coefficient_count / n_per_row;
    let n_cols = codeword_length::<S>(n_per_row);
    let level_parameters = level_parameters::<F, S>(n_per_row);
    let seed = HASH_SEED ^ u64::from(log_n) ^ params_tag;

    let setup_start = Instant::now();
    let enc = SdigEncodingS::<F, S>::new_from_dims(n_per_row, n_cols, seed);
    let graph_sampling_ns = setup_start.elapsed().as_nanos();
    if enc.get_dims(n_per_row) != (1, n_per_row, n_cols) {
        return Err("derived encoding dimensions disagree with upstream constructor".to_owned());
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

    for _ in 0..args.warmup {
        encode_matrix(&enc, &mut matrix, n_cols);
    }
    let encode_samples = sample_times(args.runs, || encode_matrix(&enc, &mut matrix, n_cols));
    let direct_root = hash_matrix::<F>(&matrix, n_rows, n_cols);

    for _ in 0..args.warmup {
        let _ = hash_matrix::<F>(&matrix, n_rows, n_cols);
    }
    let hash_samples = sample_times(args.runs, || {
        let _ = hash_matrix::<F>(&matrix, n_rows, n_cols);
    });

    let verifier_setup_start = Instant::now();
    let verifier_enc = SdigEncodingS::<F, S>::new_from_dims(n_per_row, n_cols, seed);
    let verifier_encoding_init_ns = verifier_setup_start.elapsed().as_nanos();
    let point = F::random(&mut rng);
    let (inner_tensor, outer_tensor) = tensors(point, n_per_row, n_rows);
    let expected_eval = evaluate_polynomial_tensors(&coefficients, &inner_tensor, &outer_tensor);

    for _ in 0..args.warmup {
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
    let commit_samples = sample_times(args.runs, || {
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
    let prove_samples = sample_times(args.runs, || {
        let mut transcript = make_transcript(root.as_ref(), enc.get_n_col_opens());
        proof_slot = Some(
            commit
                .prove(&outer_tensor, &enc, &mut transcript)
                .expect("LCPC prove failed during timed run"),
        );
    });
    let proof = proof_slot.expect("runs is nonzero");

    let mut verified = false;
    let mut verified_value = <F as Field>::ZERO;
    let verify_samples = sample_times(args.runs, || {
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

    Ok(BenchReport {
        schema_version: "lcpc-bench-v1",
        implementation: "lcpc-fork",
        upstream_commit: UPSTREAM_COMMIT,
        driver_version: env!("CARGO_PKG_VERSION"),
        hash_algorithm: "BLAKE3-256",
        hash_crate_version: "1.8.7",
        hash_mode: HASH_MODE,
        field: field_name,
        params: params_name.to_owned(),
        log_n,
        seed,
        threads: args.threads,
        cores: args.cores.clone(),
        warmup: args.warmup,
        runs: args.runs,
        dimensions: Dimensions {
            coefficient_count,
            n_rows,
            n_per_row,
            n_cols,
        },
        recursion_depth: level_parameters.len(),
        level_parameters,
        setup_ns: SetupTimes {
            graph_sampling_and_encoding_init_ns: graph_sampling_ns,
            input_generation_and_row_buffer_init_ns: input_setup_ns,
            verifier_encoding_init_ns,
        },
        measurements: Measurements {
            encode_matrix: summarize(encode_samples),
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
        hash_decomposition_includes_scratch_allocation: true,
    })
}

fn encode_matrix<F, S>(enc: &S, matrix: &mut [F], n_cols: usize)
where
    S: LcEncoding<F = F>,
{
    for row in matrix.chunks_mut(n_cols) {
        enc.encode(row).expect("LCPC encode failed");
    }
}

fn sample_times(mut runs: usize, mut operation: impl FnMut()) -> Vec<u128> {
    let mut samples = Vec::with_capacity(runs);
    while runs > 0 {
        let start = Instant::now();
        operation();
        samples.push(start.elapsed().as_nanos());
        runs -= 1;
    }
    samples
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
    let mean_ns = samples.iter().map(|v| *v as f64).sum::<f64>() / samples.len() as f64;
    let summary = Summary {
        min_ns: sorted[0],
        q1_ns: sorted[(sorted.len() - 1) / 4],
        median_ns,
        q3_ns: sorted[3 * (sorted.len() - 1) / 4],
        max_ns: *sorted.last().unwrap(),
        mean_ns,
    };
    Samples {
        samples_ns: samples,
        summary,
    }
}

fn hash_matrix<F>(matrix: &[F], n_rows: usize, n_cols: usize) -> Output<Blake3>
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

fn make_transcript(root: &Output<Blake3>, n_col_opens: usize) -> Transcript {
    let mut transcript = Transcript::new(b"LCPC original baseline");
    transcript.append_message(b"polycommit", root.as_slice());
    transcript.append_message(b"ncols", &(n_col_opens as u64).to_be_bytes());
    transcript
}

fn tensors<F: Field>(point: F, n_per_row: usize, n_rows: usize) -> (Vec<F>, Vec<F>) {
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

fn evaluate_polynomial_tensors<F: Field>(coefficients: &[F], inner: &[F], outer: &[F]) -> F {
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

fn ceil_muldiv(n: usize, numerator: usize, denominator: usize) -> usize {
    (n * numerator + denominator - 1) / denominator
}

/// Compute the output length from the public code specification. This is the
/// dimension recurrence used by upstream `matgen::generate`, without sampling.
fn codeword_length<S: SdigSpecification>(n_per_row: usize) -> usize {
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

fn level_parameters<F: SizedField, S: SdigSpecification>(n: usize) -> Vec<LevelParameters> {
    let mut result = Vec::new();
    let mut n_i = n;
    while n_i > S::baselen() {
        let m_i = ceil_muldiv(n_i, S::alpha_num(), S::alpha_den());
        let lower_a = ceil_muldiv(n_i, 32 * S::beta_num(), 25 * S::beta_den());
        let lower_b = 4 + ceil_muldiv(n_i, S::beta_num(), S::beta_den());
        let entropy_c = ((110.0 / n_i as f64 + S::cnst_cn_1()) / S::cnst_cn_2()).ceil() as usize;
        let c_n = lower_a.max(lower_b).min(entropy_c).min(m_i);

        let n_i_prime = ceil_muldiv(m_i, S::r_num(), S::r_den());
        let m_i_prime = ceil_muldiv(n_i, S::r_num(), S::r_den()) - n_i - n_i_prime;
        let tmp1 = ceil_muldiv(n_i, 2 * S::beta_num(), S::beta_den());
        let tmp2 = ceil_muldiv(n_i, S::r_num(), S::r_den()) - n_i + 110;
        let formula_d = tmp1 + (tmp2 as f64 / F::FLOG2 as f64).ceil() as usize;
        let entropy_d = ((110.0 / n_i as f64 + S::cnst_dn_1()) / S::cnst_dn_2()).ceil() as usize;
        let d_n = formula_d.min(entropy_d).min(m_i_prime);

        result.push(LevelParameters {
            n_i,
            m_i,
            c_n,
            n_i_prime,
            m_i_prime,
            d_n,
        });
        n_i = m_i;
    }
    result
}

trait FieldReprHex {
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

// NOTE (step/02-fields part 2): Implement `FieldReprHex` for `Fp2M61` here once added.

fn field_hex<F: FieldReprHex>(value: &F) -> String {
    value.to_repr_hex()
}

fn bytes_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn make_golden_file() -> Result<GoldenFile, String> {
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

fn golden_case<F, S>(
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
        + MulAcc
        + Num
        + SizedField
        + Serialize
        + Debug
        + Clone
        + Send
        + Sync
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

fn quoted(value: &str) -> String {
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

fn write_string_array(out: &mut String, values: &[String], indent: usize) {
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

fn write_nested_string_array(out: &mut String, values: &[Vec<String>], indent: usize) {
    out.push_str("[\n");
    for (index, row) in values.iter().enumerate() {
        write!(out, "{}", " ".repeat(indent + 2)).unwrap();
        write_string_array(out, row, indent + 2);
        writeln!(out, "{}", if index + 1 == values.len() { "" } else { "," }).unwrap();
    }
    write!(out, "{}]", " ".repeat(indent)).unwrap();
}

fn write_summary(out: &mut String, samples: &Samples, indent: usize) {
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

fn write_report(out: &mut String, report: &BenchReport, indent: usize) {
    let pad = " ".repeat(indent);
    writeln!(out, "{pad}{{").unwrap();
    macro_rules! string_field {
        ($key:literal, $value:expr) => {
            writeln!(
                out,
                "{}\"{}\": {},",
                " ".repeat(indent + 2),
                $key,
                quoted($value)
            )
            .unwrap();
        };
    }
    string_field!("schema_version", report.schema_version);
    string_field!("implementation", report.implementation);
    string_field!("upstream_commit", report.upstream_commit);
    string_field!("driver_version", report.driver_version);
    string_field!("hash_algorithm", report.hash_algorithm);
    string_field!("hash_crate_version", report.hash_crate_version);
    string_field!("hash_mode", report.hash_mode);
    string_field!("field", report.field);
    string_field!("params", &report.params);
    writeln!(out, "{}\"log_n\": {},", " ".repeat(indent + 2), report.log_n).unwrap();
    writeln!(out, "{}\"seed\": {},", " ".repeat(indent + 2), report.seed).unwrap();
    writeln!(
        out,
        "{}\"threads\": {},",
        " ".repeat(indent + 2),
        report.threads
    )
    .unwrap();
    string_field!("cores", &report.cores);
    writeln!(out, "{}\"warmup\": {},", " ".repeat(indent + 2), report.warmup).unwrap();
    writeln!(out, "{}\"runs\": {},", " ".repeat(indent + 2), report.runs).unwrap();

    writeln!(out, "{}\"dimensions\": {{", " ".repeat(indent + 2)).unwrap();
    writeln!(
        out,
        "{}\"coefficient_count\": {},",
        " ".repeat(indent + 4),
        report.dimensions.coefficient_count
    )
    .unwrap();
    writeln!(
        out,
        "{}\"n_rows\": {},",
        " ".repeat(indent + 4),
        report.dimensions.n_rows
    )
    .unwrap();
    writeln!(
        out,
        "{}\"n_per_row\": {},",
        " ".repeat(indent + 4),
        report.dimensions.n_per_row
    )
    .unwrap();
    writeln!(
        out,
        "{}\"n_cols\": {}",
        " ".repeat(indent + 4),
        report.dimensions.n_cols
    )
    .unwrap();
    writeln!(out, "{} }},", " ".repeat(indent + 2)).unwrap();
    writeln!(
        out,
        "{}\"recursion_depth\": {},",
        " ".repeat(indent + 2),
        report.recursion_depth
    )
    .unwrap();

    writeln!(out, "{}\"level_parameters\": [", " ".repeat(indent + 2)).unwrap();
    for (index, level) in report.level_parameters.iter().enumerate() {
        let comma = if index + 1 == report.level_parameters.len() {
            ""
        } else {
            ","
        };
        writeln!(out, "{}{{", " ".repeat(indent + 4)).unwrap();
        writeln!(out, "{}\"n_i\": {},", " ".repeat(indent + 6), level.n_i).unwrap();
        writeln!(out, "{}\"m_i\": {},", " ".repeat(indent + 6), level.m_i).unwrap();
        writeln!(out, "{}\"c_n\": {},", " ".repeat(indent + 6), level.c_n).unwrap();
        writeln!(
            out,
            "{}\"n_i_prime\": {},",
            " ".repeat(indent + 6),
            level.n_i_prime
        )
        .unwrap();
        writeln!(
            out,
            "{}\"m_i_prime\": {},",
            " ".repeat(indent + 6),
            level.m_i_prime
        )
        .unwrap();
        writeln!(out, "{}\"d_n\": {}", " ".repeat(indent + 6), level.d_n).unwrap();
        writeln!(out, "{}{}{comma}", " ".repeat(indent + 4), "}").unwrap();
    }
    writeln!(out, "{}],", " ".repeat(indent + 2)).unwrap();

    writeln!(out, "{}\"setup_ns\": {{", " ".repeat(indent + 2)).unwrap();
    writeln!(
        out,
        "{}\"graph_sampling_and_encoding_init_ns\": {},",
        " ".repeat(indent + 4),
        report.setup_ns.graph_sampling_and_encoding_init_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"input_generation_and_row_buffer_init_ns\": {},",
        " ".repeat(indent + 4),
        report.setup_ns.input_generation_and_row_buffer_init_ns
    )
    .unwrap();
    writeln!(
        out,
        "{}\"verifier_encoding_init_ns\": {}",
        " ".repeat(indent + 4),
        report.setup_ns.verifier_encoding_init_ns
    )
    .unwrap();
    writeln!(out, "{} }},", " ".repeat(indent + 2)).unwrap();

    writeln!(out, "{}\"measurements\": {{", " ".repeat(indent + 2)).unwrap();
    let measurements = [
        ("encode_matrix", &report.measurements.encode_matrix),
        (
            "hash_matrix_decomposition",
            &report.measurements.hash_matrix_decomposition,
        ),
        ("commit_total", &report.measurements.commit_total),
        ("prove", &report.measurements.prove),
        ("verify", &report.measurements.verify),
    ];
    for (index, (name, samples)) in measurements.iter().enumerate() {
        writeln!(out, "{}{}: {{", " ".repeat(indent + 4), quoted(name)).unwrap();
        write_summary(out, samples, indent + 6);
        writeln!(
            out,
            "{} }}{}",
            " ".repeat(indent + 4),
            if index + 1 == measurements.len() {
                ""
            } else {
                ","
            }
        )
        .unwrap();
    }
    writeln!(out, "{} }},", " ".repeat(indent + 2)).unwrap();

    writeln!(out, "{}\"correctness\": {{", " ".repeat(indent + 2)).unwrap();
    writeln!(
        out,
        "{}\"direct_encoder_root_matches_commit\": {},",
        " ".repeat(indent + 4),
        report.correctness.direct_encoder_root_matches_commit
    )
    .unwrap();
    writeln!(
        out,
        "{}\"proof_verification_passed\": {},",
        " ".repeat(indent + 4),
        report.correctness.proof_verification_passed
    )
    .unwrap();
    writeln!(
        out,
        "{}\"verified_evaluation_hex\": {}",
        " ".repeat(indent + 4),
        quoted(&report.correctness.verified_evaluation_hex)
    )
    .unwrap();
    writeln!(out, "{} }},", " ".repeat(indent + 2)).unwrap();
    writeln!(
        out,
        "{}\"hash_decomposition_includes_scratch_allocation\": {}",
        " ".repeat(indent + 2),
        report.hash_decomposition_includes_scratch_allocation
    )
    .unwrap();
    write!(out, "{pad}}}").unwrap();
}

fn benchmark_json(reports: &[BenchReport]) -> String {
    let mut out = String::from("[\n");
    for (index, report) in reports.iter().enumerate() {
        write_report(&mut out, report, 2);
        writeln!(out, "{}", if index + 1 == reports.len() { "" } else { "," }).unwrap();
    }
    out.push(']');
    out
}

fn golden_json(golden: &GoldenFile) -> String {
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
