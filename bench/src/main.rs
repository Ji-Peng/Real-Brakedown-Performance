// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

use std::fs;
use std::time::Instant;

use lcpc_brakedown_pc::codespec::CodeParams;
use lcpc_fields::{Fp2M61, Mers127, Mont127};
use lcpc_test_fields::{ft127::Ft127, ft63::Ft63};

use lcpc_bench::{
    benchmark_json, golden_json, make_golden_file, run_all_field_benchmarks,
    run_benchmark, run_ligero_rs_benchmark, write_output, BenchReport, Dimensions,
    Measurements, Samples, SetupTimes, Summary, Correctness,
    HASH_MODE, UPSTREAM_COMMIT,
};

#[derive(Debug, Clone)]
struct Args {
    field: String,
    params: String,
    shapes: Vec<String>,
    logn: Vec<u32>,
    runs: Option<usize>,
    warmup: Option<usize>,
    threads: Vec<usize>,
    cores: String,
    slices: Vec<usize>,
    autotune: bool,
    timeout_secs: u64,
    perf: bool,
    sweep: bool,
    single_case: bool,
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

    // Single-case mode for isolated worker runs
    if args.single_case {
        let shape = args.shapes.first().map(|s| s.as_str()).unwrap_or("brakedown");
        let log_n = *args.logn.first().ok_or("missing logN for --single-case")?;
        let threads = *args.threads.first().unwrap_or(&1);
        let slices = *args.slices.first().unwrap_or(&0);
        let runs = args.runs.unwrap_or(5);
        let warmup = args.warmup.unwrap_or(if log_n >= 26 { 1 } else { 2 });

        std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
        let report = run_single_case(
            &args.field,
            &args.params,
            shape,
            log_n,
            threads,
            &args.cores,
            slices,
            args.autotune,
            runs,
            warmup,
        )?;
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("cannot serialize single case: {e}"))?;
        println!("{json}");
        return Ok(());
    }

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
        let field_runs = args.runs.unwrap_or(5) * 10_000;
        let field_reports = if args.field == "all" {
            run_all_field_benchmarks(field_runs)
        } else {
            match args.field.as_str() {
                "mont127" => vec![lcpc_bench::field_microbenchmarks::<Mont127>("Mont127", field_runs)],
                "mers127" => vec![lcpc_bench::field_microbenchmarks::<Mers127>("Mers127", field_runs)],
                "fp2m61" => vec![lcpc_bench::field_microbenchmarks::<Fp2M61>("Fp2M61", field_runs)],
                "ft127" => vec![lcpc_bench::field_microbenchmarks::<Ft127>("Ft127", field_runs)],
                "ft63" => vec![lcpc_bench::field_microbenchmarks::<Ft63>("Ft63", field_runs)],
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

    let current_exe = std::env::current_exe().map_err(|e| format!("cannot find current_exe: {e}"))?;
    let mut reports: Vec<BenchReport> = Vec::new();

    let total_configs = args.shapes.len() * args.logn.len() * args.threads.len() * args.slices.len();
    let is_multi_config = total_configs > 1 || args.sweep;

    for shape in &args.shapes {
        for &log_n in &args.logn {
            for &threads in &args.threads {
                for &slices in &args.slices {
                    let runs = args.runs.unwrap_or(5);
                    let warmup = args.warmup.unwrap_or(if log_n >= 26 { 1 } else { 2 });

                    let report = if args.sweep || is_multi_config {
                        execute_worker(
                            &current_exe,
                            shape,
                            &args.field,
                            &args.params,
                            log_n,
                            threads,
                            &args.cores,
                            slices,
                            args.autotune,
                            runs,
                            warmup,
                            args.timeout_secs,
                            args.perf,
                        )
                    } else {
                        std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
                        run_single_case(
                            &args.field,
                            &args.params,
                            shape,
                            log_n,
                            threads,
                            &args.cores,
                            slices,
                            args.autotune,
                            runs,
                            warmup,
                        )?
                    };

                    let med_ms = report.measurements.encode_matrix.summary.median_ns / 1_000_000.0;
                    eprintln!(
                        "[CONFIG] shape={} field={} params={} logN={} T={} s={} -> status={} (med: {:.2} ms)",
                        report.shape, report.field, report.params, report.log_n,
                        report.threads, report.slices, report.status, med_ms
                    );

                    reports.push(report);

                    // Update output file progressively in sweep / multi-config mode
                    if let Some(path) = args.output.as_deref() {
                        let json = benchmark_json(&reports);
                        let _ = write_output(path, &json);
                    }
                }
            }
        }
    }

    let json = benchmark_json(&reports);
    if let Some(path) = args.output.as_deref() {
        write_output(path, &json)?;
    } else {
        println!("{json}");
    }
    Ok(())
}

fn run_single_case(
    field: &str,
    params_name: &str,
    shape: &str,
    log_n: u32,
    threads: usize,
    cores: &str,
    slices: usize,
    autotune: bool,
    runs: usize,
    warmup: usize,
) -> Result<BenchReport, String> {
    if params_name == "rs-ligero" {
        return run_ligero_rs_benchmark(log_n, runs, warmup);
    }
    let params = CodeParams::from_name(params_name)
        .ok_or_else(|| format!("unsupported --params {}", params_name))?;
    match field {
        "mont127" => run_benchmark::<Mont127>("Mont127", params, shape, log_n, threads, cores, slices, autotune, runs, warmup),
        "mers127" => run_benchmark::<Mers127>("Mers127", params, shape, log_n, threads, cores, slices, autotune, runs, warmup),
        "fp2m61" => run_benchmark::<Fp2M61>("Fp2M61", params, shape, log_n, threads, cores, slices, autotune, runs, warmup),
        "ft127" => run_benchmark::<Ft127>("Ft127", params, shape, log_n, threads, cores, slices, autotune, runs, warmup),
        "ft63" => run_benchmark::<Ft63>("Ft63", params, shape, log_n, threads, cores, slices, autotune, runs, warmup),
        other => Err(format!("unsupported field {other}")),
    }
}

fn make_error_report(
    shape: &str,
    field: &str,
    params: &str,
    log_n: u32,
    threads: usize,
    cores: &str,
    slices: usize,
    warmup: usize,
    runs: usize,
    status: &str,
    error_msg: &str,
) -> BenchReport {
    let coefficient_count = 1usize.checked_shl(log_n).unwrap_or(0);
    let is_orion = shape.eq_ignore_ascii_case("orion");
    let (n_rows, n_per_row) = if is_orion {
        (128, if coefficient_count >= 128 { coefficient_count / 128 } else { 0 })
    } else {
        let n_per_row = 1usize << (log_n / 2);
        (if n_per_row > 0 { coefficient_count / n_per_row } else { 0 }, n_per_row)
    };
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
    BenchReport {
        schema_version: "lcpc-bench-v1".to_string(),
        implementation: "lcpc-fork".to_string(),
        upstream_commit: UPSTREAM_COMMIT.to_string(),
        driver_version: env!("CARGO_PKG_VERSION").to_string(),
        hash_algorithm: "BLAKE3-256".to_string(),
        hash_crate_version: "1.8.7".to_string(),
        hash_mode: HASH_MODE.to_string(),
        field: field.to_string(),
        params: params.to_string(),
        shape: shape.to_string(),
        log_n,
        seed: 0,
        threads,
        cores: cores.to_string(),
        slices,
        slice_mode: if slices == 0 { "adaptive".to_string() } else { "explicit".to_string() },
        adaptive_slices: 0,
        warmup,
        runs,
        dimensions: Dimensions {
            coefficient_count,
            n_rows,
            n_per_row,
            n_cols: 0,
        },
        recursion_depth: 0,
        level_parameters: vec![],
        total_graph_edges: 0,
        total_edge_row_macs: 0,
        ns_per_mac: 0.0,
        cycles_per_mac: None,
        cpu_mhz_before: None,
        cpu_mhz_after: None,
        proof_size_bytes: 0,
        setup_ns: SetupTimes {
            graph_sampling_and_encoding_init_ns: 0,
            input_generation_and_row_buffer_init_ns: 0,
            verifier_encoding_init_ns: 0,
        },
        measurements: Measurements {
            encode_matrix: empty_samples.clone(),
            verifier_encode: empty_samples.clone(),
            verifier_encode_1: None,
            verifier_encode_2: None,
            hash_matrix_decomposition: empty_samples.clone(),
            commit_total: empty_samples.clone(),
            prove: empty_samples.clone(),
            verify: empty_samples,
        },
        correctness: Correctness {
            direct_encoder_root_matches_commit: false,
            proof_verification_passed: false,
            verified_evaluation_hex: String::new(),
        },
        peak_rss_bytes: 0,
        hash_decomposition_includes_scratch_allocation: true,
        status: status.to_string(),
        error: Some(error_msg.to_string()),
        perf_stat: None,
    }
}

fn execute_worker(
    current_exe: &std::path::Path,
    shape: &str,
    field: &str,
    params: &str,
    log_n: u32,
    threads: usize,
    cores: &str,
    slices: usize,
    autotune: bool,
    runs: usize,
    warmup: usize,
    timeout_secs: u64,
    perf: bool,
) -> BenchReport {
    let tmp_perf_file = if perf {
        Some(format!("/tmp/lcpc_perf_{}_{}_{}.csv", std::process::id(), log_n, threads))
    } else {
        None
    };

    let mut cmd = if let Some(ref perf_path) = tmp_perf_file {
        let mut c = std::process::Command::new("perf");
        c.arg("stat")
            .arg("-x,")
            .arg("-o")
            .arg(perf_path)
            .arg("-e")
            .arg("cycles,instructions,branches,branch-misses,cache-misses,LLC-load-misses")
            .arg("--")
            .arg(current_exe);
        c
    } else {
        std::process::Command::new(current_exe)
    };

    cmd.arg("--single-case")
        .arg("--shape").arg(shape)
        .arg("--field").arg(field)
        .arg("--params").arg(params)
        .arg("--logN").arg(log_n.to_string())
        .arg("--threads").arg(threads.to_string())
        .arg("--slices").arg(if slices == 0 { "adaptive".to_string() } else { slices.to_string() })
        .arg("--runs").arg(runs.to_string())
        .arg("--warmup").arg(warmup.to_string());

    if autotune {
        cmd.arg("--autotune");
    }
    if !cores.is_empty() {
        cmd.arg("--cores").arg(cores);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return make_error_report(
                shape, field, params, log_n, threads, cores, slices, warmup, runs,
                "failed", &format!("cannot spawn worker: {e}"),
            );
        }
    };

    let start = Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let poll_interval = std::time::Duration::from_millis(100);

    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(ref p) = tmp_perf_file {
                        let _ = fs::remove_file(p);
                    }
                    return make_error_report(
                        shape, field, params, log_n, threads, cores, slices, warmup, runs,
                        "timeout", &format!("execution timed out after {timeout_secs}s"),
                    );
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(ref p) = tmp_perf_file {
                    let _ = fs::remove_file(p);
                }
                return make_error_report(
                    shape, field, params, log_n, threads, cores, slices, warmup, runs,
                    "failed", &format!("error waiting for child: {e}"),
                );
            }
        }
    };

    let mut perf_stat_opt = None;
    if let Some(ref perf_path) = tmp_perf_file {
        if let Ok(perf_content) = fs::read_to_string(perf_path) {
            perf_stat_opt = Some(lcpc_bench::parse_perf_stat_output(&perf_content));
        }
        let _ = fs::remove_file(perf_path);
    }

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    let is_oom = {
        #[cfg(unix)]
        {
            status.signal() == Some(9) || status.code() == Some(137)
        }
        #[cfg(not(unix))]
        {
            status.code() == Some(137)
        }
    };

    if is_oom {
        return make_error_report(
            shape, field, params, log_n, threads, cores, slices, warmup, runs,
            "oom", "process terminated by SIGKILL (OOM)",
        );
    }

    if !status.success() {
        let mut err_str = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut err_str);
        }
        return make_error_report(
            shape, field, params, log_n, threads, cores, slices, warmup, runs,
            "failed", &format!("process exited with {status:?}: {err_str}"),
        );
    }

    let mut out_str = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out_str);
    }

    match serde_json::from_str::<BenchReport>(&out_str) {
        Ok(mut rep) => {
            if let Some(perf) = perf_stat_opt {
                rep.perf_stat = Some(perf);
            }
            rep
        }
        Err(e) => make_error_report(
            shape, field, params, log_n, threads, cores, slices, warmup, runs,
            "failed", &format!("cannot parse worker JSON output: {e}\noutput: {out_str}"),
        ),
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        field: "mont127".to_owned(),
        params: "lcpc-sdig6".to_owned(),
        shapes: vec!["brakedown".to_owned()],
        logn: vec![16, 18, 20, 22, 24, 26],
        runs: None,
        warmup: None,
        threads: vec![1],
        cores: String::new(),
        slices: vec![0],
        autotune: false,
        timeout_secs: 1800,
        perf: false,
        sweep: false,
        single_case: false,
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
                if args.params != "rs-ligero" && CodeParams::from_name(&args.params).is_none() {
                    return Err(
                        "--params must be lcpc-sdig6, lcpc-sdig3, simd-spielman, or rs-ligero".to_owned(),
                    );
                }
            }
            "--shape" => {
                let raw = value(&mut it, "--shape")?.to_ascii_lowercase();
                args.shapes = raw.split(',').map(|s| s.trim().to_string()).collect();
                for s in &args.shapes {
                    if s != "brakedown" && s != "orion" {
                        return Err(format!("--shape must be brakedown or orion (got '{s}')"));
                    }
                }
            }
            "--logN" => {
                let raw = value(&mut it, "--logN")?;
                args.logn = raw
                    .split(',')
                    .map(|v| v.trim().parse::<u32>().map_err(|_| format!("invalid logN {v}")))
                    .collect::<Result<Vec<_>, _>>()?;
                if args.logn.is_empty() || args.logn.iter().any(|n| *n < 7) {
                    return Err("--logN values must be at least 7".to_owned());
                }
            }
            "--runs" => {
                args.runs = Some(parse_usize(&value(&mut it, "--runs")?, "--runs")?);
            }
            "--warmup" => {
                args.warmup = Some(parse_usize(&value(&mut it, "--warmup")?, "--warmup")?);
            }
            "--threads" => {
                let raw = value(&mut it, "--threads")?;
                args.threads = raw
                    .split(',')
                    .map(|v| parse_usize(v.trim(), "--threads"))
                    .collect::<Result<Vec<_>, _>>()?;
                if args.threads.iter().any(|&t| t == 0) {
                    return Err("--threads must be positive".to_string());
                }
            }
            "--cores" => args.cores = value(&mut it, "--cores")?,
            "--slices" => {
                let raw = value(&mut it, "--slices")?;
                args.slices = raw
                    .split(',')
                    .map(|v| {
                        let v = v.trim();
                        if v.eq_ignore_ascii_case("adaptive") {
                            Ok(0usize)
                        } else {
                            parse_usize(v, "--slices")
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "--timeout-secs" => {
                args.timeout_secs = value(&mut it, "--timeout-secs")?
                    .parse::<u64>()
                    .map_err(|_| "--timeout-secs must be an integer".to_string())?;
            }
            "--autotune" => {
                args.autotune = true;
            }
            "--perf" => {
                args.perf = true;
            }
            "--sweep" => {
                args.sweep = true;
            }
            "--single-case" => {
                args.single_case = true;
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
    if let Some(r) = args.runs {
        if r == 0 {
            return Err("--runs must be positive".to_owned());
        }
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
           --shape brakedown|orion                         Matrix shape (default: brakedown; list supported)\n\
           --field mont127|mers127|fp2m61|ft127|ft63|all  Finite field (default: mont127)\n\
           --params lcpc-sdig6|lcpc-sdig3|simd-spielman   Code parameters (default: lcpc-sdig6)\n\
           --logN LIST                                     Comma-separated sizes (default: 16,18,20,22,24,26)\n\
           --runs N                                        Timed repetitions (default: 5)\n\
           --warmup N                                      Warm-up repetitions (default: 2, 1 at logN>=26)\n\
           --threads LIST                                  Worker threads list (default: 1)\n\
           --cores LIST                                    CPU cores for taskset pinning\n\
           --slices LIST                                   LLC slice count list (default: adaptive)\n\
           --autotune                                      Auto-tune LLC slice count empirically\n\
           --timeout-secs N                                Per-configuration timeout in seconds (default: 1800)\n\
           --perf                                          Wrap execution with perf stat PMU counters\n\
           --sweep                                         Run full parameter sweep with process isolation\n\
           --field-bench                                   Run field microbenchmarks (mul, mac, reduce)\n\
           --mode all|encode|verifier|pcs|fields|rs        Execution mode (default: all)\n\
           --output PATH                                   Write benchmark JSON report to PATH\n\
           --generate-golden PATH                          Generate golden test vectors\n\
           --check-golden PATH                             Verify golden test vectors\n\
           -h, --help                                      Show this help"
    );
}
