//! `adopt report` — wire unadopted artifacts into the docket ledger.
//!
//! For each artifact whose verdict is `not-installed` or `installed-stale`,
//! emit one `docket report` invocation with a stable key, severity, and
//! evidence refs. All artifact-derived strings are passed as discrete
//! [`Command`] arguments — never interpolated into a shell string.
//!
//! ## scion-truth: lineage-based docket reporting
//!
//! The headline finding `adopt-scan-stale-binaries` is keyed on the
//! **lineage** verdict only: artifacts with `verdict == installed-stale &&
//! freshness_basis == lineage`.  Clock-fallback artifacts (no marker) go
//! under a distinct `adopt-unmarked-installs` finding.  When the lineage
//! count reaches 0 the finding is *resolved* rather than re-parked.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::scan;
use crate::types::{ArtifactResult, FreshnessBasis, Verdict};
use crate::verify;

// ── Docket finding slugs (stable) ────────────────────────────────────────────

/// Docket finding slug for genuinely-behind (lineage-proven) binaries.
pub const SLUG_STALE_LINEAGE: &str = "adopt-scan-stale-binaries";
/// Docket finding slug for binaries with no install marker (clock-fallback).
pub const SLUG_UNMARKED: &str = "adopt-unmarked-installs";

// ── Per-artifact JSON record for --format json ────────────────────────────────

/// A single artifact's contribution to a docket finding, as emitted in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingArtifact {
    /// Binary name.
    pub bin: String,
    /// Repo path.
    pub repo: String,
    /// Adoption verdict.
    pub verdict: Verdict,
    /// Basis on which freshness was determined.
    pub freshness_basis: FreshnessBasis,
    /// Human-readable detail (e.g. "source is 9d newer").
    pub detail: String,
}

/// Arguments for [`run_report`].
pub struct ReportArgs {
    /// Caller-supplied opaque run identifier passed through to `docket report --run`.
    pub run_id: String,
    /// If true, print commands but do not execute them.
    pub dry_run: bool,
    /// If set, read a previously captured `adopt scan --format json` payload from this path.
    /// Use `"-"` to read from stdin.
    pub from_json: Option<PathBuf>,
    /// Output format; when `Json`, emit per-finding artifact lists rather than docket calls.
    pub format: ReportFormat,
}

/// Output format for `adopt report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportFormat {
    /// Run (or dry-run print) `docket` subprocess calls.
    Docket,
    /// Emit a JSON document of per-finding artifact lists.
    Json,
}

/// Retrieve the short (12-char) commit SHA of the newest `src/` commit in `repo`.
///
/// Returns `"unknown"` on any error (git not found, not a repo, no src/ commits).
fn source_sha(repo: &str) -> String {
    let out = Command::new("git")
        .args(["-C", repo, "log", "-1", "--format=%h", "--", "src/"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if s.is_empty() {
                "unknown".to_owned()
            } else {
                s
            }
        }
        _ => "unknown".to_owned(),
    }
}

/// Build the discrete argument vector for a single `docket report` call.
///
/// No artifact-derived string is interpolated into a shell expression — each
/// value is its own element in the returned `Vec<String>`.
///
/// Uses per-reason docket slugs (`adopt-stale-<reason>`) so the docket can
/// escalate each failure category independently.
///
/// Exported as `pub` for integration tests (AC2/AC4/AC5/AC8).
#[must_use]
pub fn build_docket_args(run_id: &str, artifact: &ArtifactResult) -> Vec<String> {
    // Classify to get per-reason slug.
    let classified = verify::classify(artifact, 2);
    let slug = classified.reason.docket_slug();

    let key = format!("{}:{}", slug, artifact.bin);
    let title = format!(
        "{} not current ({})",
        artifact.bin,
        classified.reason.display_name(),
    );
    let severity = if artifact.is_daemon { "error" } else { "warn" };
    let sha = source_sha(&artifact.repo);
    let evidence_path = format!("path:{}", artifact.repo);
    let evidence_commit = format!("commit:{}", sha);

    vec![
        "report".to_owned(),
        "--run".to_owned(),
        run_id.to_owned(),
        "--key".to_owned(),
        key,
        "--title".to_owned(),
        title,
        "--severity".to_owned(),
        severity.to_owned(),
        "--evidence".to_owned(),
        evidence_path,
        "--evidence".to_owned(),
        evidence_commit,
    ]
}

/// Build the discrete argument vector for a `docket resolve` call.
///
/// Used when the lineage-behind count reaches zero to auto-resolve the
/// `adopt-scan-stale-binaries` finding.
#[must_use]
pub fn build_resolve_args(run_id: &str, slug: &str) -> Vec<String> {
    vec![
        "resolve".to_owned(),
        "--run".to_owned(),
        run_id.to_owned(),
        "--key".to_owned(),
        slug.to_owned(),
    ]
}

/// Returns the `StaleReason` docket slug for a single artifact.
///
/// Convenience for callers that only need the slug string.
#[must_use]
pub fn docket_slug_for(artifact: &ArtifactResult) -> &'static str {
    verify::classify(artifact, 2).reason.docket_slug()
}

/// Load artifacts from a JSON file or stdin.
///
/// Pass `path = "-"` to read from stdin.
///
/// # Errors
/// Returns an error if the path cannot be read or the JSON is malformed.
fn artifacts_from_path(path: &Path) -> Result<Vec<ArtifactResult>> {
    let content = if path == Path::new("-") {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading artifacts JSON from stdin")?;
        buf
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("reading artifacts JSON from {}", path.display()))?
    };
    serde_json::from_str(&content).context("parsing artifacts JSON")
}

/// Partition artifacts into three buckets:
///
/// - `lineage_stale`:   `installed-stale` with `freshness_basis == lineage`  → genuinely behind
/// - `clock_fallback`:  `installed-stale` with `freshness_basis == clock-fallback` → unmarked
/// - `not_installed`:   `not-installed` verdict → never installed
///
/// All other verdicts (`installed-current`, `not-a-bin`) are ignored.
#[must_use]
pub fn partition_artifacts(
    artifacts: &[ArtifactResult],
) -> (Vec<&ArtifactResult>, Vec<&ArtifactResult>, Vec<&ArtifactResult>) {
    let mut lineage_stale = Vec::new();
    let mut clock_fallback = Vec::new();
    let mut not_installed = Vec::new();

    for a in artifacts {
        match (&a.verdict, &a.freshness_basis) {
            (Verdict::InstalledStale, FreshnessBasis::Lineage) => lineage_stale.push(a),
            (Verdict::InstalledStale, FreshnessBasis::ClockFallback) => clock_fallback.push(a),
            (Verdict::NotInstalled, _) => not_installed.push(a),
            _ => {} // InstalledCurrent, NotABin — skip
        }
    }

    (lineage_stale, clock_fallback, not_installed)
}

/// Invoke (or dry-run print) a `docket` subcommand.
fn call_docket(docket_args: &[String], dry_run: bool) -> Result<()> {
    if dry_run {
        let display: Vec<String> = std::iter::once("docket".to_owned())
            .chain(docket_args.iter().map(|a| shell_quote(a)))
            .collect();
        #[allow(clippy::print_stdout)]
        {
            println!("{}", display.join(" "));
        }
    } else {
        let status = Command::new("docket")
            .args(docket_args)
            .status()
            .context("spawning docket")?;
        if !status.success() {
            bail!("docket exited with status {status}");
        }
    }
    Ok(())
}

/// Run `adopt report`.
///
/// Scans for unadopted artifacts (or reads a prior scan from `--from-json`)
/// and emits one `docket report` invocation per actionable artifact.
///
/// With scion-truth semantics:
/// - `installed-stale + lineage`        → `adopt-scan-stale-binaries` (genuinely behind)
/// - `installed-stale + clock-fallback` → `adopt-unmarked-installs` (needs marker)
/// - lineage count == 0                 → resolve `adopt-scan-stale-binaries`
/// - `not-installed`                    → per-reason slug (unchanged)
///
/// # Errors
/// Returns an error if `docket` is not on `$PATH`, the scan fails, or any
/// subprocess returns a non-zero exit status.
pub fn run_report(args: ReportArgs) -> Result<()> {
    // Verify docket is on PATH before doing any work.
    if which_docket().is_none() {
        bail!(
            "docket not found on PATH; to install: adopt apply --execute --only docket"
        );
    }

    let artifacts: Vec<ArtifactResult> = match args.from_json {
        Some(ref path) => artifacts_from_path(path)?,
        None => scan::run_scan(true, None)?,
    };

    let (lineage_stale, clock_fallback, not_installed) = partition_artifacts(&artifacts);

    if args.format == ReportFormat::Json {
        return emit_json(&lineage_stale, &clock_fallback, &not_installed);
    }

    // ── Report not-installed artifacts (unchanged behavior) ──────────────────
    for artifact in &not_installed {
        let docket_args = build_docket_args(&args.run_id, artifact);
        call_docket(&docket_args, args.dry_run)?;
    }

    // ── Report genuinely-behind (lineage-stale) under headline finding ───────
    for artifact in &lineage_stale {
        let sha = source_sha(&artifact.repo);
        let key = format!("{}:{}", SLUG_STALE_LINEAGE, artifact.bin);
        let title = format!("{} genuinely behind (lineage)", artifact.bin);
        let severity = if artifact.is_daemon { "error" } else { "warn" };
        let evidence_path = format!("path:{}", artifact.repo);
        let evidence_commit = format!("commit:{sha}");
        let docket_args = vec![
            "report".to_owned(),
            "--run".to_owned(),
            args.run_id.clone(),
            "--key".to_owned(),
            key,
            "--title".to_owned(),
            title,
            "--severity".to_owned(),
            severity.to_owned(),
            "--evidence".to_owned(),
            evidence_path,
            "--evidence".to_owned(),
            evidence_commit,
        ];
        call_docket(&docket_args, args.dry_run)?;
    }

    // ── Auto-resolve headline finding when lineage count == 0 ────────────────
    if lineage_stale.is_empty() {
        let resolve_args = build_resolve_args(&args.run_id, SLUG_STALE_LINEAGE);
        call_docket(&resolve_args, args.dry_run)?;
    }

    // ── Report unmarked (clock-fallback stale) under separate finding ─────────
    for artifact in &clock_fallback {
        let sha = source_sha(&artifact.repo);
        let key = format!("{}:{}", SLUG_UNMARKED, artifact.bin);
        let title = format!("{} needs lineage marker (clock-fallback)", artifact.bin);
        let severity = if artifact.is_daemon { "error" } else { "warn" };
        let evidence_path = format!("path:{}", artifact.repo);
        let evidence_commit = format!("commit:{sha}");
        let docket_args = vec![
            "report".to_owned(),
            "--run".to_owned(),
            args.run_id.clone(),
            "--key".to_owned(),
            key,
            "--title".to_owned(),
            title,
            "--severity".to_owned(),
            severity.to_owned(),
            "--evidence".to_owned(),
            evidence_path,
            "--evidence".to_owned(),
            evidence_commit,
        ];
        call_docket(&docket_args, args.dry_run)?;
    }

    Ok(())
}

/// Emit JSON output for `adopt report --format json`.
///
/// Emits an object with three keys:
/// - `lineage_stale`:   genuinely-behind artifacts
/// - `clock_fallback`:  artifacts needing a marker
/// - `not_installed`:   missing artifacts
///
/// Each entry includes `freshness_basis` per AC4.
///
/// # Errors
/// Returns an error if serialization fails.
fn emit_json(
    lineage_stale: &[&ArtifactResult],
    clock_fallback: &[&ArtifactResult],
    not_installed: &[&ArtifactResult],
) -> Result<()> {
    let to_finding: fn(&ArtifactResult) -> FindingArtifact = |a| {
        let detail = match (&a.verdict, a.source_commit_ts, a.installed_ts) {
            (Verdict::InstalledStale, Some(src), Some(inst)) => {
                let days = (src - inst) / 86400;
                format!("source is {days}d newer than installed binary")
            }
            (Verdict::InstalledStale, _, _) => {
                "source HEAD is newer than installed binary".to_owned()
            }
            _ => "not installed".to_owned(),
        };
        FindingArtifact {
            bin: a.bin.clone(),
            repo: a.repo.clone(),
            verdict: a.verdict.clone(),
            freshness_basis: a.freshness_basis.clone(),
            detail,
        }
    };

    #[derive(Serialize)]
    struct JsonReport {
        lineage_stale: Vec<FindingArtifact>,
        clock_fallback: Vec<FindingArtifact>,
        not_installed: Vec<FindingArtifact>,
    }

    let report = JsonReport {
        lineage_stale: lineage_stale.iter().map(|a| to_finding(a)).collect(),
        clock_fallback: clock_fallback.iter().map(|a| to_finding(a)).collect(),
        not_installed: not_installed.iter().map(|a| to_finding(a)).collect(),
    };

    #[allow(clippy::print_stdout)]
    {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

/// Locate the `docket` binary on `$PATH`.
///
/// Returns `Some(path)` if found, `None` otherwise.
fn which_docket() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("docket");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Minimally shell-quote a string for dry-run display only.
///
/// Not used for actual subprocess execution — this is purely cosmetic.
fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "-_./+:@=".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
