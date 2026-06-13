//! CLI argument parsing and command dispatch.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};

use adopt::apply;
use adopt::doctor;
use adopt::{reconcile, report, scan, verify};

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

        /// Bypass the incremental-skip check and reinstall all artifacts regardless
        /// of whether their source fingerprint has changed.
        #[arg(long)]
        force_all: bool,
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

        /// Output format: `docket` (default) runs docket subcommands; `json` emits a JSON
        /// document with per-finding artifact lists including `freshness_basis`.
        #[arg(long, default_value = "docket")]
        format: ReportOutputFormat,
    },

    /// Detect and optionally clean adopt-created junk under literal-tilde prefixes.
    Doctor {
        /// Remove junk binaries that have a verified twin in ~/.local/bin or ~/.cargo/bin.
        /// Junk with no twin is reported but never deleted.
        #[arg(long)]
        clean: bool,
    },

    /// Classify not-current artifacts into named failure buckets.
    Verify {
        /// Output format.
        #[arg(long, default_value = "table")]
        format: OutputFormat,

        /// Day threshold for splitting SourceNewer: artifacts with delta >= N days are
        /// classified as SourceNewer-behind; those below are SourceNewer-sameday.
        #[arg(long, default_value = "2", value_name = "N")]
        behind_days: i64,
    },

    /// Mint lineage markers for installed-but-unmarked binaries (no rebuild).
    Reconcile {
        /// Print planned actions without writing any markers.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

/// Output format for `adopt report`.
#[derive(Clone, Debug, ValueEnum)]
enum ReportOutputFormat {
    /// Run (or dry-run print) `docket` subprocess calls.
    Docket,
    /// Emit a JSON document with per-finding artifact lists (includes `freshness_basis`).
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
        Command::Apply { execute, with_daemons, only, force_all } => {
            let dry_run = !execute;
            let results =
                apply::run_apply(dry_run, execute, only.as_deref(), with_daemons, force_all)?;
            print_apply_results(&results);

            // Exit non-zero if any artifact failed or had a bad prefix.
            let any_failed = results.iter().any(|r| {
                matches!(
                    r.verdict,
                    apply::ApplyOutcome::Failed { .. } | apply::ApplyOutcome::BadPrefix { .. }
                )
            });
            if any_failed {
                bail!("one or more installs failed");
            }
        }
        Command::Report { run, dry_run, from_json, format } => {
            let report_format = match format {
                ReportOutputFormat::Docket => report::ReportFormat::Docket,
                ReportOutputFormat::Json => report::ReportFormat::Json,
            };
            report::run_report(report::ReportArgs {
                run_id: run,
                dry_run,
                from_json,
                format: report_format,
            })?;
        }
        Command::Doctor { clean } => {
            let any_debris = doctor::run_doctor(clean)?;
            if any_debris {
                bail!("adopt doctor: junk debris detected under a literal-tilde prefix");
            }
        }
        Command::Verify { format, behind_days } => {
            let fmt = match format {
                OutputFormat::Table => verify::VerifyFormat::Table,
                OutputFormat::Json => verify::VerifyFormat::Json,
            };
            let any_not_current = verify::run_verify(verify::VerifyArgs {
                format: fmt,
                behind_days,
            })?;
            if any_not_current {
                bail!("verify: one or more artifacts are not current");
            }
        }
        Command::Reconcile { dry_run } => {
            let results = reconcile::run_reconcile(dry_run)?;
            reconcile::print_reconcile_results(&results, dry_run);
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

    println!("{:<25} {:<35} {:>8}", "BIN", "OUTCOME", "MS");
    let sep = "-".repeat(72_usize);
    println!("{sep}");

    for r in results {
        let outcome = match &r.verdict {
            apply::ApplyOutcome::InstalledOk => "installed-ok".to_owned(),
            apply::ApplyOutcome::InstalledCurrent => "installed-current".to_owned(),
            apply::ApplyOutcome::AlreadyCurrent => "already-current (skipped)".to_owned(),
            apply::ApplyOutcome::SkippedDaemon { note } => {
                format!("skipped-daemon: {note}")
            }
            apply::ApplyOutcome::SkippedDaemonsNotRequested => {
                "skipped-rollout-absent".to_owned()
            }
            apply::ApplyOutcome::RolloutDelegated => "rollout-delegated".to_owned(),
            apply::ApplyOutcome::Failed { reason } => format!("FAILED: {reason}"),
            apply::ApplyOutcome::NoRollout => "no-rollout".to_owned(),
            apply::ApplyOutcome::BadPrefix { resolved } => {
                format!("bad-prefix: {resolved}")
            }
        };
        println!("{:<25} {:<35} {:>8}", r.bin, outcome, r.elapsed_ms);
    }

    // Summary line distinguishing installed / already-current / skipped-daemon / failed.
    println!("{sep}");
    let installed: usize = results
        .iter()
        .filter(|r| matches!(r.verdict, apply::ApplyOutcome::InstalledOk))
        .count();
    let already_current: usize = results
        .iter()
        .filter(|r| {
            matches!(
                r.verdict,
                apply::ApplyOutcome::AlreadyCurrent | apply::ApplyOutcome::InstalledCurrent
            )
        })
        .count();
    let skipped_daemon: usize = results
        .iter()
        .filter(|r| {
            matches!(
                r.verdict,
                apply::ApplyOutcome::SkippedDaemon { .. }
                    | apply::ApplyOutcome::SkippedDaemonsNotRequested
            )
        })
        .count();
    let failed: usize = results
        .iter()
        .filter(|r| {
            matches!(
                r.verdict,
                apply::ApplyOutcome::Failed { .. } | apply::ApplyOutcome::BadPrefix { .. }
            )
        })
        .count();
    println!(
        "Summary: installed={installed}  already-current={already_current}  \
         skipped-daemon={skipped_daemon}  failed={failed}"
    );
}
