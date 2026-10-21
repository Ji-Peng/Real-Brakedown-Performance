// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

use std::fs;

use lcpc_brakedown_pc::codespec::CodeParams;
use lcpc_fields::{Fp2M61, Mers127, Mont127};
use lcpc_test_fields::{ft127::Ft127, ft63::Ft63};

use lcpc_bench::{
    benchmark_json, golden_json, make_golden_file, run_all_field_benchmarks, run_benchmark,
    write_output, BenchReport,
};

#[derive(Debug, Clone)]
struct Args {
    field: String,
    params: String,
    logn: Vec<u32>,
    runs: usize,
    warmup: usize,
    threads: usize,
    cores: String,
    slices: usize,
    autotune: bool,
    output: Option<String>,
    generate_golden: Option<String>,
    check_golden: Option<String>,
    bench_fields: bool,
    mode: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lcpc-bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // Ensure non-inlined field kernel symbols are retained in the binary for `scripts/check_codegen.py --strict`
    std::hint::black_box((
        lcpc_fields::mont127::mac_column_mont127 as *const (),
        lcpc_fields::mont127::reduce_column_mont127 as *const (),
        lcpc_fields::mers127::mac_column_mers127 as *const (),
        lcpc_fields::mers127::reduce_column_mers127 as *const (),
        lcpc_fields::fp2m61::mac_column_fp2_m61 as *const (),
        lcpc_fields::fp2m61::reduce_column_fp2_m61 as *const (),
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

    if args.bench_fields || args.mode == "fields" {
        let field_reports = if args.field == "all" {
            run_all_field_benchmarks(args.runs * 10_000)
        } else {
            match args.field.as_str() {
                "mont127" => vec![lcpc_bench::field_microbenchmarks::<Mont127>(
                    "Mont127",
                    args.runs * 10_000,
                )],
                "mers127" => vec![lcpc_bench::field_microbenchmarks::<Mers127>(
                    "Mers127",
                    args.runs * 10_000,
                )],
                "fp2m61" => vec![lcpc_bench::field_microbenchmarks::<Fp2M61>(
                    "Fp2M61",
                    args.runs * 10_000,
                )],
                "ft127" => vec![lcpc_bench::field_microbenchmarks::<Ft127>(
                    "Ft127",
                    args.runs * 10_000,
                )],
                other => return Err(format!("unsupported field for microbench: {other}")),
            }
        };
        let json = serde_json::to_string_pretty(&field_reports)
            .map_err(|e| format!("cannot serialize field microbenchmarks: {e}"))?;
        if let Some(path) = args.output.as_deref() {
            write_output(path, &json)?;
        } else {
            println!("{json}");
        }
        return Ok(());
    }

    let mut reports: Vec<BenchReport> = Vec::with_capacity(args.logn.len());
    for &log_n in &args.logn {
        let params = CodeParams::from_name(&args.params)
            .ok_or_else(|| format!("unsupported --params {}", args.params))?;
        let report = match args.field.as_str() {
            "mont127" => run_benchmark::<Mont127>(
                "Mont127",
                params,
                log_n,
                args.threads,
                &args.cores,
                args.slices,
                args.autotune,
                args.runs,
                args.warmup,
            )?,
            "mers127" => run_benchmark::<Mers127>(
                "Mers127",
                params,
                log_n,
                args.threads,
                &args.cores,
                args.slices,
                args.autotune,
                args.runs,
                args.warmup,
            )?,
            "fp2m61" => run_benchmark::<Fp2M61>(
                "Fp2M61",
                params,
                log_n,
                args.threads,
                &args.cores,
                args.slices,
                args.autotune,
                args.runs,
                args.warmup,
            )?,
            "ft127" => run_benchmark::<Ft127>(
                "Ft127",
                params,
                log_n,
                args.threads,
                &args.cores,
                args.slices,
                args.autotune,
                args.runs,
                args.warmup,
            )?,
            "ft63" => run_benchmark::<Ft63>(
                "Ft63",
                params,
                log_n,
                args.threads,
                &args.cores,
                args.slices,
                args.autotune,
                args.runs,
                args.warmup,
            )?,
            other => return Err(format!("unsupported field {other}")),
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
        slices: 0,
        autotune: false,
        output: None,
        generate_golden: None,
        check_golden: None,
        bench_fields: false,
        mode: "all".to_owned(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let value = |it: &mut std::iter::Skip<std::env::Args>, name: &str| {
            it.next().ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--field" => {
                args.field = value(&mut it, "--field")?.to_ascii_lowercase();
                if !matches!(
                    args.field.as_str(),
                    "mont127" | "mers127" | "fp2m61" | "ft127" | "ft63" | "all"
                ) {
                    return Err(
                        "--field must be mont127, mers127, fp2m61, ft127, ft63, or all".to_owned(),
                    );
                }
            }
            "--params" => {
                args.params = value(&mut it, "--params")?;
                if CodeParams::from_name(&args.params).is_none() {
                    return Err(
                        "--params must be lcpc-sdig6, lcpc-sdig3, or simd-spielman".to_owned(),
                    );
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
            "--slices" => {
                let raw = value(&mut it, "--slices")?;
                if raw.eq_ignore_ascii_case("adaptive") {
                    args.slices = 0;
                } else {
                    args.slices = parse_usize(&raw, "--slices")?;
                }
            }
            "--autotune" => {
                args.autotune = true;
            }
            "--output" => args.output = Some(value(&mut it, "--output")?),
            "--generate-golden" => args.generate_golden = Some(value(&mut it, "--generate-golden")?),
            "--check-golden" => args.check_golden = Some(value(&mut it, "--check-golden")?),
            "--field-bench" => args.bench_fields = true,
            "--mode" => args.mode = value(&mut it, "--mode")?,
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

fn parse_usize(raw: &str, name: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn print_help() {
    println!(
        "Usage: lcpc-bench [options]\n\
         \n\
         Options:\n\
           --field mont127|mers127|fp2m61|ft127|ft63|all  Finite field (default: mont127)\n\
           --params lcpc-sdig6|lcpc-sdig3|simd-spielman   Code parameters (default: lcpc-sdig6)\n\
           --logN LIST                                     Comma-separated even sizes (default: 16,18,20,22,24,26)\n\
           --runs N                                        Timed repetitions (default: 30)\n\
           --warmup N                                      Warm-up repetitions (default: 5)\n\
           --threads N                                     Worker threads (default: 1)\n\
           --cores LIST                                    CPU cores for taskset pinning\n\
           --slices N|adaptive                             LLC slice count (default: adaptive)\n\
           --autotune                                      Auto-tune LLC slice count empirically\n\
           --field-bench                                   Run field microbenchmarks (mul, mac, reduce)\n\
           --mode all|encode|verifier|pcs|fields           Execution mode (default: all)\n\
           --output PATH                                   Write benchmark JSON report to PATH\n\
           --generate-golden PATH                          Generate golden test vectors\n\
           --check-golden PATH                             Verify golden test vectors\n\
           -h, --help                                      Show this help"
    );
}
