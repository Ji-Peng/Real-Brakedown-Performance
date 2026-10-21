// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Dedicated field microbenchmark binary (`mul`, `mac`, `reduce`).
//!
//! Reports ns/op and throughput across Mont127, Mers127, Fp2M61, and Ft127.

use lcpc_bench::{field_microbenchmarks, run_all_field_benchmarks, write_output};
use lcpc_fields::{Fp2M61, Mers127, Mont127};
use lcpc_test_fields::{ft127::Ft127, ft63::Ft63};

fn main() {
    if let Err(e) = run() {
        eprintln!("bench_fields: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut field = "all".to_string();
    let mut runs = 100_000usize;
    let mut output: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--field" => {
                field = it
                    .next()
                    .ok_or_else(|| "--field requires a value".to_string())?
                    .to_ascii_lowercase();
            }
            "--runs" | "--iterations" => {
                let val = it
                    .next()
                    .ok_or_else(|| "--runs requires an integer value".to_string())?;
                runs = val
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --runs: {val}"))?;
            }
            "--output" => {
                output = Some(
                    it.next()
                        .ok_or_else(|| "--output requires a path".to_string())?,
                );
            }
            "-h" | "--help" => {
                println!(
                    "Usage: bench_fields [--field mont127|mers127|fp2m61|ft127|all] [--runs N] [--output PATH]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown option {other}")),
        }
    }

    let reports = if field == "all" {
        run_all_field_benchmarks(runs)
    } else {
        match field.as_str() {
            "mont127" => vec![field_microbenchmarks::<Mont127>("Mont127", runs)],
            "mers127" => vec![field_microbenchmarks::<Mers127>("Mers127", runs)],
            "fp2m61" => vec![field_microbenchmarks::<Fp2M61>("Fp2M61", runs)],
            "ft127" => vec![field_microbenchmarks::<Ft127>("Ft127", runs)],
            "ft63" => vec![field_microbenchmarks::<Ft63>("Ft63", runs)],
            other => return Err(format!("unsupported field {other}")),
        }
    };

    let json = serde_json::to_string_pretty(&reports)
        .map_err(|e| format!("cannot serialize field microbenchmarks: {e}"))?;

    if let Some(path) = output {
        write_output(&path, &json)?;
        println!("Field microbenchmarks written to {path}");
    } else {
        println!("{json}");
    }

    Ok(())
}
