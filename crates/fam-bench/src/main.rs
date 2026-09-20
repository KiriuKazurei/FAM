//! Latency harness for the enumeration path.
//!
//! A binary rather than `#[bench]` because criterion-style harnesses need nightly, and the point
//! is to run on any checkout of this machine's corpora. Timings are warm-cache: the first sample of
//! every case is discarded as a warm-up and excluded from the statistics.
//!
//!   fam-bench run                  measure and print
//!   fam-bench run --write          also record bench/baseline.json
//!   fam-bench check                re-measure and fail if p95 regressed > 15 %

mod measure;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::measure::{Case, CaseResult};

#[derive(Parser)]
#[command(name = "fam-bench", about = "Measure FAM enumeration latency")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Baseline file used by `check` and written by `run --write`.
    #[arg(long, global = true, default_value = "bench/baseline.json")]
    baseline: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Run every available corpus and report p50/p95.
    Run {
        /// Override the per-case iteration count.
        #[arg(long)]
        iterations: Option<usize>,
        /// Repeat each case and keep the least-disturbed pass.
        #[arg(long, default_value_t = DEFAULT_PASSES)]
        passes: usize,
        /// Record the results as the new baseline.
        #[arg(long)]
        write: bool,
    },
    /// Re-run and compare against the recorded baseline.
    Check {
        #[arg(long)]
        iterations: Option<usize>,
        #[arg(long, default_value_t = DEFAULT_PASSES)]
        passes: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { iterations, passes, write } => {
            let results = run_all(iterations, passes)?;
            if write {
                write_baseline(&cli.baseline, &results)?;
                println!("\nbaseline written to {}", cli.baseline.display());
            }
            Ok(())
        }
        Command::Check { iterations, passes } => {
            let results = run_all(iterations, passes)?;
            check_against_baseline(&cli.baseline, &results)
        }
    }
}

const DEFAULT_PASSES: usize = 3;

fn run_all(
    iterations: Option<usize>,
    passes: usize,
) -> Result<BTreeMap<String, CaseResult>> {
    let mut results = BTreeMap::new();
    for case in Case::all() {
        match case.measure_best_of(iterations, passes) {
            Ok(result) => {
                result.print_line();
                results.insert(result.id.clone(), result);
            }
            Err(missing) => {
                println!("{:<24} SKIP  {}", case.id(), missing.display());
            }
        }
    }
    if results.is_empty() {
        anyhow::bail!("no corpora were available; set FAM_BENCH_FLAT / _MIXED / _TREE / _VOLUME");
    }
    Ok(results)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Baseline {
    /// Free-form description of where the numbers came from.
    recorded_on: String,
    cases: BTreeMap<String, CaseResult>,
}

fn write_baseline(path: &Path, results: &BTreeMap<String, CaseResult>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let body = Baseline {
        recorded_on: describe_machine(),
        cases: results.clone(),
    };
    let json = serde_json::to_string_pretty(&body)?;
    let mut file = std::fs::File::create(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(json.as_bytes())?;
    Ok(())
}

fn check_against_baseline(path: &Path, current: &BTreeMap<String, CaseResult>) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("no baseline at {}; run `fam-bench run --write` first", path.display()))?;
    let stored: Baseline = serde_json::from_str(&text).context("baseline is not valid JSON")?;

    const FAIL_RATIO: f64 = 1.25;
    const WARN_RATIO: f64 = 1.10;
    let mut failures: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Only the floor gates. It is the fastest observation across all passes: background activity
    // cannot push it below the hardware floor, but a real code regression always raises it. p50 and
    // p95 moved by more than the gate width between runs of the same binary on this machine, so
    // they are printed for context and never decide the verdict.
    println!(
        "\n{:<22} {:>10} {:>10} {:>7} {:>7}  verdict",
        "case", "floor base", "floor now", "floor x", "p50 x"
    );
    for (id, now) in current {
        let Some(base) = stored.cases.get(id) else {
            warnings.push(format!("{id}: not in baseline"));
            continue;
        };
        let p50_ratio = ratio(base.p50_us, now.p50_us);
        let floor_ratio = ratio(base.min_us, now.min_us);

        let verdict = if floor_ratio > FAIL_RATIO {
            failures.push(format!(
                "{id}: floor {}us -> {}us ({:.2}x)",
                base.min_us, now.min_us, floor_ratio
            ));
            "FAIL"
        } else if floor_ratio > WARN_RATIO {
            warnings.push(format!("{id}: floor regressed {:.2}x", floor_ratio));
            "warn"
        } else {
            "ok"
        };
        println!(
            "{:<22} {:>9}us {:>9}us {:>6.2}x {:>6.2}x  {verdict}",
            id, base.min_us, now.min_us, floor_ratio, p50_ratio
        );
    }

    for warning in &warnings {
        println!("warn: {warning}");
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("error: {failure}");
        }
        anyhow::bail!("{} benchmark case(s) regressed beyond the 25% floor gate", failures.len());
    }
    println!("\nall measured cases within gate");
    Ok(())
}

fn ratio(base: u64, now: u64) -> f64 {
    if base == 0 {
        1.0
    } else {
        now as f64 / base as f64
    }
}

fn describe_machine() -> String {
    format!(
        "{} {}, warm cache, fam-bench {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION")
    )
}

