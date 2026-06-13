//! `adopt report` — wire unadopted artifacts into the docket ledger.
//!
//! For each artifact whose verdict is `not-installed` or `installed-stale`,
//! emit one `docket report` invocation with a stable key, severity, and
//! evidence refs. All artifact-derived strings are passed as discrete
//! [`Command`] arguments — never interpolated into a shell string.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::scan;
use crate::types::ArtifactResult;
use crate::verify;

/// Arguments for [`run_report`].
pub struct ReportArgs {
    /// Caller-supplied opaque run identifier passed through to `docket report --run`.
    pub run_id: String,
    /// If true, print commands but do not execute them.
    pub dry_run: bool,
    /// If set, read a previously captured `adopt scan --format json` payload from this path.
    /// Use `"-"` to read from stdin.
    pub from_json: Option<PathBuf>,
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
    let classified = verify::classify(artifact);
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

/// Returns the `StaleReason` docket slug for a single artifact.
///
/// Convenience for callers that only need the slug string.
#[must_use]
pub fn docket_slug_for(artifact: &ArtifactResult) -> &'static str {
    verify::classify(artifact).reason.docket_slug()
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

/// Run `adopt report`.
///
/// Scans for unadopted artifacts (or reads a prior scan from `--from-json`)
/// and emits one `docket report` invocation per actionable artifact.
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

    for artifact in &artifacts {
        if !artifact.verdict.is_actionable() {
            continue;
        }

        let docket_args = build_docket_args(&args.run_id, artifact);

        if args.dry_run {
            // Print the command as a shell-renderable string for human inspection.
            let display: Vec<String> = std::iter::once("docket".to_owned())
                .chain(docket_args.iter().map(|a| shell_quote(a)))
                .collect();
            // Allowed in dry-run output path.
            #[allow(clippy::print_stdout)]
            {
                println!("{}", display.join(" "));
            }
        } else {
            let status = Command::new("docket")
                .args(&docket_args)
                .status()
                .context("spawning docket")?;
            if !status.success() {
                bail!("docket report exited with status {status}");
            }
        }
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
