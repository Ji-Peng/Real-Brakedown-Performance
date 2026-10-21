// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Dedicated end-to-end PCS (Commit / Prove / Verify) benchmark binary.
//!
//! Evaluates the 2-D polynomial commitment scheme stages:
//! - Commit latency with decomposition: batched row encoding vs column hashing + Merkle tree.
//! - Proof generation latency.
//! - Verification latency.

use lcpc_bench::{benchmark_json, run_benchmark, write_output, BenchReport};
use lcpc_brakedown_pc::codespec::CodeParams;
use lcpc_fields::{Fp2M61, Mers127, Mont127};
use lcpc_test_fields::{ft127::Ft127, ft63::Ft63};

fn main() {
    if let Err(e) = run() {
        eprintln!("bench_pcs: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut field = "mont127".to_string();
    let mut params_str = "lcpc-sdig6".to_string();
    let mut shape = "brakedown".to_string();
    let mut logn: Vec<u32> = vec![16, 18, 20, 22, 24, 26];
    let mut runs: Option<usize> = None;
    let mut warmup: Option<usize> = None;
    let mut threads = 1usize;
    let mut cores = String::new();
    let mut slices = 0usize;
    let mut autotune = false;
    let mut output: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--shape" => {
                shape = it
                    .next()
                    .ok_or_else(|| "--shape requires a value".to_string())?
                    .to_ascii_lowercase();
            }
            "--field" => {
                field = it
                    .next()
                    .ok_or_else(|| "--field requires a value".to_string())?
                    .to_ascii_lowercase();
            }
            "--params" => {
                params_str = it
                    .next()
                    .ok_or_else(|| "--params requires a value".to_string())?;
            }
            "--logN" => {
                let raw = it
                    .next()
                    .ok_or_else(|| "--logN requires a value".to_string())?;
                logn = raw
                    .split(',')
                    .map(|v| v.parse::<u32>().map_err(|_| format!("invalid logN {v}")))
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "--runs" => {
                let v = it.next().ok_or_else(|| "--runs requires a value")?;
                runs = Some(v.parse::<usize>().map_err(|_| "invalid --runs")?);
            }
            "--warmup" => {
                let v = it.next().ok_or_else(|| "--warmup requires a value")?;
                warmup = Some(v.parse::<usize>().map_err(|_| "invalid --warmup")?);
            }
            "--threads" => {
                let v = it.next().ok_or_else(|| "--threads requires a value")?;
                threads = v.parse::<usize>().map_err(|_| "invalid --threads")?;
            }
            "--cores" => {
                cores = it.next().ok_or_else(|| "--cores requires a value")?;
            }
            "--slices" => {
                let raw = it.next().ok_or_else(|| "--slices requires a value")?;
                if raw.eq_ignore_ascii_case("adaptive") {
                    slices = 0;
                } else {
                    slices = raw.parse::<usize>().map_err(|_| "invalid --slices")?;
                }
            }
            "--autotune" => {
                autotune = true;
            }
            "--output" => {
                output = Some(it.next().ok_or_else(|| "--output requires a value")?);
            }
            "-h" | "--help" => {
                println!(
                    "Usage: bench_pcs [--field mont127|mers127|fp2m61|ft127|ft63] \\\n\
                     [--params lcpc-sdig6|lcpc-sdig3|simd-spielman] \\\n\
                     [--logN 16,18,...] [--runs N] [--warmup N] [--threads N] \\\n\
                     [--cores LIST] [--slices N|adaptive] [--autotune] [--output PATH]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown option {other}")),
        }
    }

    std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
    let params = CodeParams::from_name(&params_str)
        .ok_or_else(|| format!("unsupported --params {params_str}"))?;

    let mut reports: Vec<BenchReport> = Vec::with_capacity(logn.len());
    for &log_n in &logn {
        let eff_runs = runs.unwrap_or(5);
        let eff_warmup = warmup.unwrap_or(if log_n >= 26 { 1 } else { 2 });
        let report = match field.as_str() {
            "mont127" => run_benchmark::<Mont127>(
                "Mont127", params, &shape, log_n, threads, &cores, slices, autotune, eff_runs, eff_warmup,
            )?,
            "mers127" => run_benchmark::<Mers127>(
                "Mers127", params, &shape, log_n, threads, &cores, slices, autotune, eff_runs, eff_warmup,
            )?,
            "fp2m61" => run_benchmark::<Fp2M61>(
                "Fp2M61", params, &shape, log_n, threads, &cores, slices, autotune, eff_runs, eff_warmup,
            )?,
            "ft127" => run_benchmark::<Ft127>(
                "Ft127", params, &shape, log_n, threads, &cores, slices, autotune, eff_runs, eff_warmup,
            )?,
            "ft63" => run_benchmark::<Ft63>(
                "Ft63", params, &shape, log_n, threads, &cores, slices, autotune, eff_runs, eff_warmup,
            )?,
            other => return Err(format!("unsupported field {other}")),
        };
        reports.push(report);
    }

    let json = benchmark_json(&reports);
    if let Some(path) = output {
        write_output(&path, &json)?;
    } else {
        println!("{json}");
    }

    Ok(())
}
