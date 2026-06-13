//! CLI argument parsing and command dispatch.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};

use adopt::apply;
use adopt::{report, scan};

/// Detect shipped wintermute artifacts that never entered the live system.
#[derive(Parser)]
#[command(name = "adopt", version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan wintermute repos for unadopted or stale artifacts.
    Scan {
        /// Output format.
        #[arg(long, default_value = "table")]
        format: OutputFormat,

        /// Include installed-current and not-a-bin entries in output.
        #[arg(long)]
        all: bool,

        /// Restrict output to artifacts whose bin name matches this regex.
        #[arg(long, value_name = "REGEX")]
        r#match: Option<String>,
    },

    /// Install unadopted non-daemon artifacts (dry-run by default).
    Apply {
        /// Actually perform the installs (default is dry-run).
        #[arg(long)]
        execute: bool,

        /// Also handle daemon artifacts by delegating to `rollout install`.
        #[arg(long)]
        with_daemons: bool,

        /// Restrict apply to a single artifact by binary name.
        #[arg(long, value_name = "BIN")]
        only: Option<String>,
    },

    /// Report unadopted artifacts to the docket ledger.
    Report {
        /// Caller-supplied opaque run identifier (e.g. `2026-06-13.1`).
        #[arg(long, value_name = "RUN_ID")]
        run: String,

        /// Print docket commands without executing them.
        #[arg(long)]
        dry_run: bool,

        /// Read a previously captured `adopt scan --format json` payload instead of re-running scan.
        /// Use `-` to read from stdin.
        #[arg(long, value_name = "FILE")]
        from_json: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

/// Entry point called from `main`.
///
/// # Errors
/// Returns an error if scanning or output fails.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Scan { format, all, r#match } => {
            let results = scan::run_scan(all, r#match.as_deref())?;
            match format {
                OutputFormat::Table => scan::print_table(&results),
                OutputFormat::Json => scan::print_json(&results)?,
            }
        }
        Command::Apply { execute, with_daemons, only } => {
            let dry_run = !execute;
            let results =
                apply::run_apply(dry_run, execute, only.as_deref(), with_daemons)?;
            print_apply_results(&results);

            // Exit non-zero if any artifact failed.
            let any_failed = results.iter().any(|r| {
                matches!(r.verdict, apply::ApplyOutcome::Failed { .. })
            });
            if any_failed {
                bail!("one or more installs failed");
            }
        }
        Command::Report { run, dry_run, from_json } => {
            report::run_report(report::ReportArgs {
                run_id: run,
                dry_run,
                from_json,
            })?;
        }
    }
    Ok(())
}

/// Prints a human-readable summary of apply results.
#[allow(clippy::print_stdout)]
fn print_apply_results(results: &[apply::ApplyResult]) {
    if results.is_empty() {
        println!("Nothing to apply.");
        return;
    }

    println!("{:<25} {:<30} {:>8}", "BIN", "OUTCOME", "MS");
    let sep = "-".repeat(68_usize);
    println!("{sep}");

    for r in results {
        let outcome = match &r.verdict {
            apply::ApplyOutcome::InstalledOk => "installed-ok".to_owned(),
            apply::ApplyOutcome::InstalledCurrent => "installed-current".to_owned(),
            apply::ApplyOutcome::SkippedDaemon { note } => {
                format!("skipped-daemon: {note}")
            }
            apply::ApplyOutcome::SkippedDaemonsNotRequested => {
                "skipped-rollout-absent".to_owned()
            }
            apply::ApplyOutcome::RolloutDelegated => "rollout-delegated".to_owned(),
            apply::ApplyOutcome::Failed { reason } => format!("FAILED: {reason}"),
            apply::ApplyOutcome::NoRollout => "no-rollout".to_owned(),
        };
        println!("{:<25} {:<30} {:>8}", r.bin, outcome, r.elapsed_ms);
    }
}
