//! `adopt converge` — convergence ledger for the fixpoint pipeline.
//!
//! After each `adopt verify` run, callers may append a [`ConvergeRecord`] to a
//! JSONL ledger at `~/.local/state/adopt/converge.jsonl`.  The ledger is then
//! read by `adopt converge` to show a trend table and optionally emit a
//! `fixpoint-not-converging` docket finding when the pipeline stalls.
//!
//! Design constraints:
//! - Append is idempotent per `run` id (same run-id → no duplicate line).
//! - File writes use `O_APPEND` so concurrent writers don't corrupt the ledger.
//! - All I/O is SIGPIPE-safe (no unwrapped `println!` in hot paths).

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Public types ──────────────────────────────────────────────────────────────

/// Docket finding slug emitted when the pipeline is not converging.
pub const SLUG_NOT_CONVERGING: &str = "fixpoint-not-converging";

/// One convergence snapshot appended after each pipeline run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvergeRecord {
    /// Opaque run identifier (e.g. `"2026-06-13.1"`).  De-duplication key.
    pub run: String,
    /// ISO-8601 UTC timestamp of the run.
    pub ts: String,
    /// Total number of scanned artifacts.
    pub total: u32,
    /// Count of `SourceNewer-behind` artifacts (genuinely behind).
    pub behind: u32,
    /// Count of artifacts blocked because their working tree is dirty.
    pub dirty_blocked: u32,
    /// Count of artifacts whose freshness used clock-fallback (no marker).
    pub fallback: u32,
    /// Count of artifacts with a current lineage marker.
    pub lineage_current: u32,
}

/// Alert emitted by [`check_convergence`] when the pipeline is not converging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvergenceAlert {
    /// Human-readable explanation.
    pub message: String,
    /// The `behind` value from the most recent record.
    pub behind: u32,
    /// Whether this is a stall (flat non-zero) rather than a regression.
    pub is_stall: bool,
}

// ── Ledger I/O ────────────────────────────────────────────────────────────────

/// Append `record` to `path` (JSONL, O_APPEND).
///
/// If a line with the same `run` id is already present the call is a no-op
/// (idempotent per run id).
///
/// The parent directory is created with `0o755` permissions if absent.
///
/// # Errors
/// Returns an error if the directory or file cannot be created / written.
pub fn append_record(record: &ConvergeRecord, path: &Path) -> Result<()> {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating ledger dir {}", parent.display()))?;
        }
    }

    // Check for duplicate run id before writing.
    if path.exists() {
        let existing = read_records(path, None)?;
        if existing.iter().any(|r| r.run == record.run) {
            return Ok(());
        }
    }

    // Serialize and append.
    let mut line = serde_json::to_string(record).context("serializing ConvergeRecord")?;
    line.push('\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening ledger {}", path.display()))?;

    file.write_all(line.as_bytes())
        .with_context(|| format!("appending to ledger {}", path.display()))?;

    Ok(())
}

/// Read the last `limit` records from `path`.
///
/// If `limit` is `None`, all records are returned.  Malformed lines are
/// silently skipped.
///
/// # Errors
/// Returns an error if the file cannot be opened or read.
pub fn read_records(path: &Path, limit: Option<usize>) -> Result<Vec<ConvergeRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)
        .with_context(|| format!("opening ledger {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut records: Vec<ConvergeRecord> = Vec::new();
    for line in reader.lines() {
        let line = line.with_context(|| format!("reading ledger {}", path.display()))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<ConvergeRecord>(trimmed) {
            records.push(rec);
        }
        // Silently skip malformed lines.
    }

    if let Some(n) = limit {
        let len = records.len();
        if len > n {
            records = records.into_iter().skip(len - n).collect();
        }
    }

    Ok(records)
}

// ── Convergence check ─────────────────────────────────────────────────────────

/// Check whether the pipeline is converging.
///
/// Returns `Some(alert)` if:
/// - `behind` strictly increased from the previous run to the last run, OR
/// - `behind > 0` for more than `stall_runs` consecutive runs at the tail.
///
/// Returns `None` when:
/// - There are fewer than 2 records (not enough history).
/// - `behind` reached 0 in the most recent record.
/// - `behind` is decreasing run-over-run.
#[must_use]
pub fn check_convergence(
    records: &[ConvergeRecord],
    stall_runs: usize,
) -> Option<ConvergenceAlert> {
    if records.len() < 2 {
        return None;
    }

    let last = records.last()?;

    // Converged — no alert regardless of history.
    if last.behind == 0 {
        return None;
    }

    let prev = records.get(records.len().wrapping_sub(2))?;

    // Regression: behind increased.
    if last.behind > prev.behind {
        return Some(ConvergenceAlert {
            message: format!(
                "behind count increased from {} to {} (regression)",
                prev.behind, last.behind
            ),
            behind: last.behind,
            is_stall: false,
        });
    }

    // Stall check: last stall_runs entries all have behind > 0 AND behind is
    // not strictly decreasing across the tail.  A decreasing trend is progress
    // (converging toward 0) — it should not be misclassified as a stall.
    if stall_runs == 0 {
        return None;
    }
    let tail_start = records.len().saturating_sub(stall_runs);
    let tail = &records[tail_start..];
    let all_nonzero = !tail.is_empty() && tail.iter().all(|r| r.behind > 0);
    // Check that the tail is NOT strictly monotonically decreasing.
    // If every consecutive pair in the tail has behind[i] > behind[i+1], it's
    // a converging sequence, not a stall.
    let is_strictly_decreasing = tail.windows(2).all(|w| w[0].behind > w[1].behind);
    if all_nonzero && tail.len() >= stall_runs && !is_strictly_decreasing {
        return Some(ConvergenceAlert {
            message: format!(
                "behind has been > 0 for {} consecutive runs (stall)",
                tail.len()
            ),
            behind: last.behind,
            is_stall: true,
        });
    }

    None
}

// ── Docket integration ────────────────────────────────────────────────────────

/// Emit or resolve the `fixpoint-not-converging` docket finding.
///
/// - `alert == Some(_)` → call `docket report` with the finding.
/// - `alert == None`    → call `docket resolve` to clear the finding.
///
/// `dry_run = true` prints the command rather than executing it.
///
/// # Errors
/// Returns an error if `docket` is not found or exits non-zero.
pub fn emit_convergence_finding(
    run_id: &str,
    alert: Option<&ConvergenceAlert>,
    dry_run: bool,
) -> Result<()> {
    let args: Vec<String> = match alert {
        Some(a) => vec![
            "report".to_owned(),
            "--run".to_owned(),
            run_id.to_owned(),
            "--key".to_owned(),
            SLUG_NOT_CONVERGING.to_owned(),
            "--title".to_owned(),
            format!("fixpoint pipeline not converging: behind={}", a.behind),
            "--severity".to_owned(),
            "warn".to_owned(),
            "--evidence".to_owned(),
            format!("detail:{}", a.message),
        ],
        None => vec![
            "resolve".to_owned(),
            "--run".to_owned(),
            run_id.to_owned(),
            "--key".to_owned(),
            SLUG_NOT_CONVERGING.to_owned(),
        ],
    };

    if dry_run {
        let display: Vec<String> =
            std::iter::once("docket".to_owned()).chain(args.iter().cloned()).collect();
        #[allow(clippy::print_stdout)]
        {
            println!("{}", display.join(" "));
        }
    } else {
        let status = Command::new("docket")
            .args(&args)
            .status()
            .context("spawning docket")?;
        if !status.success() {
            anyhow::bail!("docket exited with status {status}");
        }
    }

    Ok(())
}

// ── Trend display ─────────────────────────────────────────────────────────────

/// Trend marker comparing current vs previous value.
fn trend_marker(current: u32, prev: Option<u32>) -> char {
    match prev {
        None => '=',
        Some(p) => {
            if current > p {
                '▲'
            } else if current < p {
                '▼'
            } else {
                '='
            }
        }
    }
}

/// Print the convergence ledger as a human-readable trend table.
///
/// # Errors
/// Returns an error if stdout write fails.
#[allow(clippy::print_stdout)]
pub fn print_trend_table(records: &[ConvergeRecord]) -> Result<()> {
    if records.is_empty() {
        println!("No convergence records found.");
        return Ok(());
    }

    println!(
        "{:<22} {:>7} {:>8} {:>14} {:>10} {:>17}",
        "RUN", "TOTAL", "BEHIND", "DIRTY-BLOCKED", "FALLBACK", "LINEAGE-CURRENT"
    );
    let sep = "-".repeat(85_usize);
    println!("{sep}");

    let mut prev: Option<&ConvergeRecord> = None;
    for rec in records {
        let t = trend_marker(rec.total, prev.map(|p| p.total));
        let b = trend_marker(rec.behind, prev.map(|p| p.behind));
        let d = trend_marker(rec.dirty_blocked, prev.map(|p| p.dirty_blocked));
        let f = trend_marker(rec.fallback, prev.map(|p| p.fallback));
        let l = trend_marker(rec.lineage_current, prev.map(|p| p.lineage_current));

        println!(
            "{:<22} {:>5}{} {:>6}{} {:>12}{} {:>8}{} {:>15}{}",
            rec.run,
            rec.total, t,
            rec.behind, b,
            rec.dirty_blocked, d,
            rec.fallback, f,
            rec.lineage_current, l,
        );
        prev = Some(rec);
    }

    Ok(())
}

/// Emit records as a JSON array.
///
/// # Errors
/// Returns an error if serialization fails.
#[allow(clippy::print_stdout)]
pub fn print_trend_json(records: &[ConvergeRecord]) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(records)?);
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn make_rec(run: &str, behind: u32) -> ConvergeRecord {
        ConvergeRecord {
            run: run.to_owned(),
            ts: "2026-06-13T00:00:00Z".to_owned(),
            total: 10,
            behind,
            dirty_blocked: 0,
            fallback: 0,
            lineage_current: 10 - behind,
        }
    }

    #[test]
    fn no_alert_when_behind_zero() {
        let records = vec![make_rec("r1", 3), make_rec("r2", 0)];
        assert!(check_convergence(&records, 4).is_none());
    }

    #[test]
    fn alert_on_regression() {
        let records = vec![make_rec("r1", 2), make_rec("r2", 5)];
        let alert = check_convergence(&records, 4).expect("should alert");
        assert!(!alert.is_stall);
        assert_eq!(alert.behind, 5);
    }

    #[test]
    fn no_alert_when_decreasing() {
        let records = vec![make_rec("r1", 5), make_rec("r2", 3)];
        assert!(check_convergence(&records, 4).is_none());
    }

    #[test]
    fn stall_alert_after_n_runs() {
        let records = vec![
            make_rec("r1", 3),
            make_rec("r2", 3),
            make_rec("r3", 3),
            make_rec("r4", 3),
        ];
        let alert = check_convergence(&records, 4).expect("stall alert");
        assert!(alert.is_stall);
    }

    #[test]
    fn no_stall_below_threshold() {
        let records = vec![make_rec("r1", 3), make_rec("r2", 3), make_rec("r3", 3)];
        // stall_runs = 4 → only 3 tail entries → no stall.
        assert!(check_convergence(&records, 4).is_none());
    }

    #[test]
    fn trend_marker_directions() {
        assert_eq!(trend_marker(5, Some(3)), '▲');
        assert_eq!(trend_marker(3, Some(5)), '▼');
        assert_eq!(trend_marker(3, Some(3)), '=');
        assert_eq!(trend_marker(3, None), '=');
    }
}
