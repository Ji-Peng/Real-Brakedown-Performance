// Copyright 2026 RealBrakedownPerf Authors.
// Licensed under Apache-2.0.

//! Stable timing and statistics loop shared by `reproduce/steps.sh` at every tag.

mod adapter;

use ff::Field;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::Instant;

const DRIVER_VERSION: &str = "steps-driver-v1";
const INPUT_SEED_BASE: u64 = 0x5354_4550_5349_4e50;
const CODE_SEED_BASE: u64 = 0x5354_4550_5343_4f44;

#[derive(Debug)]
struct Args {
    mode: String,
    log_n: u32,
    runs: usize,
    warmup: usize,
    tag: String,
    session_id: String,
    commit: String,
    source_lock_sha256: String,
    driver_lock_sha256: String,
    rustc: String,
    run_class: String,
    output: Option<String>,
}

#[derive(Serialize)]
struct Record {
    schema_version: &'static str,
    harness_version: &'static str,
    adapter_version: &'static str,
    status: &'static str,
    run_class: String,
    watermark: Option<&'static str>,
    tag: String,
    session_id: String,
    commit: String,
    mode: String,
    params: &'static str,
    field: &'static str,
    log_n: u32,
    coefficient_count: usize,
    n_rows: usize,
    n_per_row: usize,
    n_cols: usize,
    threads: usize,
    slices_policy: &'static str,
    slices_used: usize,
    code_seed: u64,
    input_seed: u64,
    warmup: usize,
    runs: usize,
    setup_ns: u128,
    matrix_allocation_ns: u128,
    input_preparation_ns: u128,
    samples_ns: Vec<u128>,
    min_ns: u128,
    median_ns: f64,
    max_ns: u128,
    mean_ns: f64,
    timed_operation: &'static str,
    source_lock_sha256: String,
    driver_lock_sha256: String,
    rustc: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("steps-driver: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if adapter::MODE != "runtime" && args.mode != adapter::MODE {
        return Err(format!(
            "adapter {} supports mode {}, not {}",
            adapter::ADAPTER_VERSION,
            adapter::MODE,
            args.mode
        ));
    }

    let coefficient_count = 1usize
        .checked_shl(args.log_n)
        .ok_or_else(|| format!("logN={} overflows usize", args.log_n))?;
    if args.log_n < 10 || args.log_n % 2 != 0 {
        return Err("logN must be even and at least 10".to_owned());
    }
    if args.runs == 0 {
        return Err("runs must be greater than zero".to_owned());
    }

    let n_per_row = 1usize << (args.log_n / 2);
    let n_rows = coefficient_count / n_per_row;
    let mode_discriminator = if args.mode == "a" { 0x41u64 } else { 0x42u64 };
    let code_seed = CODE_SEED_BASE ^ u64::from(args.log_n) ^ mode_discriminator;
    let input_seed = INPUT_SEED_BASE ^ u64::from(args.log_n);

    let setup_start = Instant::now();
    let encoder = adapter::Encoder::new(&args.mode, n_per_row, code_seed)?;
    let setup_ns = setup_start.elapsed().as_nanos();
    let n_cols = encoder.n_cols();
    if n_cols <= n_per_row {
        return Err(format!("invalid code dimensions: {n_per_row} -> {n_cols}"));
    }
    let matrix_len = n_rows
        .checked_mul(n_cols)
        .ok_or_else(|| "encoded matrix dimensions overflow usize".to_owned())?;

    let allocation_start = Instant::now();
    let mut matrix = vec![<adapter::F as Field>::ZERO; matrix_len];
    let matrix_allocation_ns = allocation_start.elapsed().as_nanos();

    let mut input_preparation_ns = 0u128;
    for _ in 0..args.warmup {
        input_preparation_ns +=
            prepare_inputs::<adapter::F>(&mut matrix, n_cols, n_per_row, input_seed);
        encoder.encode_matrix(&mut matrix, n_cols)?;
        std::hint::black_box(matrix.as_slice());
    }

    let mut samples_ns = Vec::with_capacity(args.runs);
    for _ in 0..args.runs {
        input_preparation_ns +=
            prepare_inputs::<adapter::F>(&mut matrix, n_cols, n_per_row, input_seed);
        let start = Instant::now();
        encoder.encode_matrix(&mut matrix, n_cols)?;
        samples_ns.push(start.elapsed().as_nanos());
        std::hint::black_box(matrix.as_slice());
    }

    let (min_ns, median_ns, max_ns, mean_ns) = summarize(&samples_ns);
    let record = Record {
        schema_version: "steps-bench-v1",
        harness_version: DRIVER_VERSION,
        adapter_version: adapter::ADAPTER_VERSION,
        status: "ok",
        run_class: args.run_class.clone(),
        watermark: if args.run_class == "local-smoke-not-a-result" {
            Some("LOCAL SMOKE — NOT A RESULT")
        } else {
            None
        },
        tag: args.tag,
        session_id: args.session_id,
        commit: args.commit,
        mode: args.mode.clone(),
        params: if args.mode == "a" {
            "lcpc-sdig6"
        } else {
            "simd-spielman"
        },
        field: adapter::FIELD_NAME,
        log_n: args.log_n,
        coefficient_count,
        n_rows,
        n_per_row,
        n_cols,
        threads: 1,
        slices_policy: adapter::SLICES_POLICY,
        slices_used: encoder.slices_used(n_rows),
        code_seed,
        input_seed,
        warmup: args.warmup,
        runs: args.runs,
        setup_ns,
        matrix_allocation_ns,
        input_preparation_ns,
        samples_ns,
        min_ns,
        median_ns,
        max_ns,
        mean_ns,
        timed_operation: "one full prover matrix encoding; graph setup, matrix allocation, and deterministic input restoration are outside the timer",
        source_lock_sha256: args.source_lock_sha256,
        driver_lock_sha256: args.driver_lock_sha256,
        rustc: args.rustc,
    };
    let json = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
    if let Some(path) = args.output {
        if let Some(parent) = Path::new(&path).parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, json).map_err(|error| format!("cannot write {path}: {error}"))?;
    } else {
        println!(
            "{}",
            String::from_utf8(json).map_err(|error| error.to_string())?
        );
    }
    Ok(())
}

fn prepare_inputs<F: Field>(matrix: &mut [F], n_cols: usize, n_per_row: usize, seed: u64) -> u128 {
    let start = Instant::now();
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    for row in matrix.chunks_mut(n_cols) {
        for value in &mut row[..n_per_row] {
            *value = F::random(&mut rng);
        }
    }
    start.elapsed().as_nanos()
}

fn summarize(samples: &[u128]) -> (u128, f64, u128, f64) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 0 {
        (sorted[middle - 1] as f64 + sorted[middle] as f64) / 2.0
    } else {
        sorted[middle] as f64
    };
    let mean = samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
    (sorted[0], median, *sorted.last().unwrap(), mean)
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        mode: String::new(),
        log_n: 0,
        runs: 5,
        warmup: 2,
        tag: String::new(),
        session_id: String::new(),
        commit: String::new(),
        source_lock_sha256: String::new(),
        driver_lock_sha256: String::new(),
        rustc: String::new(),
        run_class: "campaign".to_owned(),
        output: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(option) = it.next() {
        let value = |it: &mut std::iter::Skip<std::env::Args>, name: &str| {
            it.next().ok_or_else(|| format!("{name} requires a value"))
        };
        match option.as_str() {
            "--mode" => args.mode = value(&mut it, "--mode")?,
            "--logN" => {
                args.log_n = value(&mut it, "--logN")?
                    .parse()
                    .map_err(|_| "invalid --logN".to_owned())?;
            }
            "--runs" => {
                args.runs = value(&mut it, "--runs")?
                    .parse()
                    .map_err(|_| "invalid --runs".to_owned())?;
            }
            "--warmup" => {
                args.warmup = value(&mut it, "--warmup")?
                    .parse()
                    .map_err(|_| "invalid --warmup".to_owned())?;
            }
            "--tag" => args.tag = value(&mut it, "--tag")?,
            "--session-id" => args.session_id = value(&mut it, "--session-id")?,
            "--commit" => args.commit = value(&mut it, "--commit")?,
            "--source-lock-sha256" => {
                args.source_lock_sha256 = value(&mut it, "--source-lock-sha256")?
            }
            "--driver-lock-sha256" => {
                args.driver_lock_sha256 = value(&mut it, "--driver-lock-sha256")?
            }
            "--rustc" => args.rustc = value(&mut it, "--rustc")?,
            "--run-class" => args.run_class = value(&mut it, "--run-class")?,
            "--output" => args.output = Some(value(&mut it, "--output")?),
            "-h" | "--help" => {
                println!("steps-driver --mode a|b --logN N [--warmup N] [--runs N] --tag TAG --session-id ID --commit SHA --output FILE");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if !matches!(args.mode.as_str(), "a" | "b") {
        return Err("--mode must be a or b".to_owned());
    }
    if args.log_n == 0
        || args.tag.is_empty()
        || args.commit.is_empty()
        || args.session_id.is_empty()
    {
        return Err("--logN, --tag, --session-id, and --commit are required".to_owned());
    }
    Ok(args)
}
