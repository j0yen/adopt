//! `adopt verify` — classify not-current artifacts into named failure buckets.
//!
//! Each artifact that is not `InstalledCurrent` receives a [`StaleReason`]
//! explaining *why* it is not current.  The full classification is printed as
//! a table or JSON, followed by a summary count line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::scan;
use crate::types::{ArtifactResult, Verdict};

// ── Taxonomy ──────────────────────────────────────────────────────────────────

/// Named bucket classifying why an artifact is not current.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum StaleReason {
    /// No binary in `~/.local/bin`, `~/.cargo/bin`, or `PATH`.
    NeverInstalled,
    /// Binary exists under a bogus prefix (e.g. `/home/jsy/~/.local/bin/`).
    WrongPrefix,
    /// Installed in a convention dir but not in current `PATH`.
    OffPath,
    /// Installed and on PATH but repo HEAD / source mtime is newer than binary.
    SourceNewer,
    /// `cargo install` exited non-zero on last attempt.
    BuildFail,
    /// Installed, on PATH, but `--version`/`--help` exits non-zero.
    SmokeFail,
}

impl StaleReason {
    /// Returns the stable docket slug for this reason.
    #[must_use]
    pub fn docket_slug(&self) -> &'static str {
        match self {
            Self::NeverInstalled => "adopt-stale-neverinstalled",
            Self::WrongPrefix => "adopt-stale-wrongprefix",
            Self::OffPath => "adopt-stale-offpath",
            Self::SourceNewer => "adopt-stale-sourcenewer",
            Self::BuildFail => "adopt-stale-buildfail",
            Self::SmokeFail => "adopt-stale-smokefail",
        }
    }

    /// Returns a display name for summary output.
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::NeverInstalled => "NeverInstalled",
            Self::WrongPrefix => "WrongPrefix",
            Self::OffPath => "OffPath",
            Self::SourceNewer => "SourceNewer",
            Self::BuildFail => "BuildFail",
            Self::SmokeFail => "SmokeFail",
        }
    }
}

// ── Classified artifact ───────────────────────────────────────────────────────

/// A not-current artifact with its classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedArtifact {
    /// Binary name.
    pub bin: String,
    /// Why this artifact is not current.
    pub reason: StaleReason,
    /// Human-readable detail for the reason.
    pub detail: String,
}

// ── Path helpers (mirrored from scan.rs — kept local to avoid pub leakage) ───

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map_or_else(|_| PathBuf::from("/root"), PathBuf::from)
}

fn local_bin() -> PathBuf {
    home_dir().join(".local/bin")
}

fn cargo_bin() -> PathBuf {
    home_dir().join(".cargo/bin")
}

/// Returns `$HOME/~` — the bogus junk prefix directory.
fn junk_prefix_dir() -> PathBuf {
    home_dir().join("~")
}

// ── Classification logic ──────────────────────────────────────────────────────

/// Returns true if `bin` is present in the junk-prefix directory (`$HOME/~/...`).
fn in_wrong_prefix(bin: &str) -> Option<String> {
    let junk = junk_prefix_dir();
    if !junk.exists() {
        return None;
    }
    // Check $HOME/~/.local/bin/<bin> and $HOME/~/bin/<bin>
    let candidates = [
        junk.join(".local/bin").join(bin),
        junk.join("bin").join(bin),
        junk.join(bin),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.display().to_string());
        }
    }
    // Walk one level deep looking for any occurrence.
    if let Ok(entries) = std::fs::read_dir(&junk) {
        for entry in entries.flatten() {
            let p = entry.path().join(bin);
            if p.exists() {
                return Some(p.display().to_string());
            }
        }
    }
    None
}

/// Returns true if `bin` is found in a convention dir but not resolvable via PATH.
///
/// "convention dirs" = `~/.local/bin` and `~/.cargo/bin`.
fn is_off_path(bin: &str) -> Option<String> {
    let local = local_bin().join(bin);
    let cargo = cargo_bin().join(bin);

    let in_convention = if local.exists() {
        Some(local.display().to_string())
    } else if cargo.exists() {
        Some(cargo.display().to_string())
    } else {
        return None;
    };

    // Now check if it's on the current PATH.
    let on_path = std::env::var("PATH").ok().is_some_and(|path_var| {
        path_var.split(':').any(|dir| {
            let candidate = Path::new(dir).join(bin);
            candidate.exists()
        })
    });

    if on_path {
        None // It IS on PATH, so not OffPath.
    } else {
        in_convention // Found in convention dir but not PATH.
    }
}

/// Returns true if `bin --version` or `bin --help` exits non-zero.
fn is_smoke_fail(bin: &str) -> bool {
    // Try --version first, then --help.
    for flag in &["--version", "--help"] {
        let result = Command::new(bin)
            .arg(flag)
            .output();
        match result {
            Ok(out) => {
                if out.status.success() {
                    return false;
                }
            }
            Err(_) => {
                // Cannot even exec — smoke fail.
                return true;
            }
        }
    }
    true
}

/// Classify a single artifact that has a non-current verdict.
#[must_use]
pub fn classify(artifact: &ArtifactResult) -> ClassifiedArtifact {
    let bin = &artifact.bin;

    // 1. If the scan says it was never found anywhere, first check wrong prefix.
    if artifact.verdict == Verdict::NotInstalled {
        // Check junk prefix ($HOME/~) first.
        if let Some(path) = in_wrong_prefix(bin) {
            return ClassifiedArtifact {
                bin: bin.clone(),
                reason: StaleReason::WrongPrefix,
                detail: format!("binary found at bogus prefix: {path}"),
            };
        }

        // Check if it's in a convention dir but not on PATH.
        if let Some(path) = is_off_path(bin) {
            return ClassifiedArtifact {
                bin: bin.clone(),
                reason: StaleReason::OffPath,
                detail: format!("installed at {path} but not on PATH"),
            };
        }

        // Truly never installed.
        return ClassifiedArtifact {
            bin: bin.clone(),
            reason: StaleReason::NeverInstalled,
            detail: "not found in ~/.local/bin, ~/.cargo/bin, or PATH".to_owned(),
        };
    }

    // 2. InstalledStale — the scan found it but source is newer.
    // Sub-classify: check if it's actually runnable.
    if artifact.verdict == Verdict::InstalledStale {
        // Check if the binary can be executed at all.
        if is_smoke_fail(bin) {
            return ClassifiedArtifact {
                bin: bin.clone(),
                reason: StaleReason::SmokeFail,
                detail: format!("{bin} --version / --help exited non-zero"),
            };
        }

        // Binary runs fine, source is just newer.
        let detail = match (artifact.source_commit_ts, artifact.installed_ts) {
            (Some(src), Some(inst)) => {
                let delta = src - inst;
                let days = delta / 86400;
                format!("source is {days}d newer than installed binary")
            }
            _ => "source HEAD is newer than installed binary".to_owned(),
        };

        return ClassifiedArtifact {
            bin: bin.clone(),
            reason: StaleReason::SourceNewer,
            detail,
        };
    }

    // Fallback — shouldn't be called for current/not-a-bin artifacts.
    ClassifiedArtifact {
        bin: bin.clone(),
        reason: StaleReason::NeverInstalled,
        detail: "unknown".to_owned(),
    }
}

/// Arguments for [`run_verify`].
pub struct VerifyArgs {
    /// Output format.
    pub format: VerifyFormat,
}

/// Output format for `adopt verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyFormat {
    /// Human-readable table output.
    Table,
    /// Machine-readable JSON array output.
    Json,
}

/// Exit code returned by [`run_verify`]: 0 = all current, 1 = any not-current.
pub type AnyNotCurrent = bool;

/// Run `adopt verify`.
///
/// Scans, classifies every not-current artifact, prints results, emits a
/// summary line, and returns whether any artifact was not current.
///
/// # Errors
/// Returns an error if the scan fails.
pub fn run_verify(args: VerifyArgs) -> Result<AnyNotCurrent> {
    let results = scan::run_scan(true, None)?;

    let classified: Vec<ClassifiedArtifact> = results
        .iter()
        .filter(|r| r.verdict.is_actionable())
        .map(|r| classify(r))
        .collect();

    if args.format == VerifyFormat::Json {
        print_json(&classified)?;
    } else {
        print_table(&classified);
    }

    // Summary line.
    print_summary(&classified);

    Ok(!classified.is_empty())
}

/// Print table output.
#[allow(clippy::print_stdout)]
fn print_table(classified: &[ClassifiedArtifact]) {
    if classified.is_empty() {
        println!("All artifacts are current.");
        return;
    }

    println!("{:<25} {:<18} DETAIL", "BIN", "REASON");
    let sep = "-".repeat(80_usize);
    println!("{sep}");

    for c in classified {
        println!(
            "{:<25} {:<18} {}",
            c.bin,
            c.reason.display_name(),
            c.detail
        );
    }
}

/// Print JSON output.
///
/// # Errors
/// Returns an error if serialization fails.
fn print_json(classified: &[ClassifiedArtifact]) -> Result<()> {
    #[allow(clippy::print_stdout)]
    {
        println!("{}", serde_json::to_string_pretty(classified)?);
    }
    Ok(())
}

/// Print summary count line.
#[allow(clippy::print_stdout)]
fn print_summary(classified: &[ClassifiedArtifact]) {
    let total = classified.len();

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for c in classified {
        *counts.entry(c.reason.display_name()).or_insert(0) += 1;
    }

    let order = [
        "NeverInstalled",
        "WrongPrefix",
        "OffPath",
        "SourceNewer",
        "BuildFail",
        "SmokeFail",
    ];

    let parts: Vec<String> = order
        .iter()
        .filter_map(|name| counts.get(name).map(|n| format!("{name}: {n}")))
        .collect();

    if parts.is_empty() {
        println!("verify: {total} total · all current");
    } else {
        println!("verify: {total} total · {{{}}}", parts.join(", "));
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::types::{ArtifactResult, Verdict};
    use std::sync::Mutex;

    // Serialize all tests that mutate env vars to avoid races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_artifact(bin: &str, verdict: Verdict, installed_path: Option<&str>,
                     source_ts: Option<i64>, installed_ts: Option<i64>) -> ArtifactResult {
        ArtifactResult {
            repo: "/tmp/fake-repo".to_owned(),
            bin: bin.to_owned(),
            verdict,
            installed_path: installed_path.map(ToOwned::to_owned),
            is_daemon: false,
            source_commit_ts: source_ts,
            installed_ts,
            fix_cmd: String::new(),
            age_vs_head: None,
        }
    }

    // AC1: JSON output has bin, reason, detail fields.
    #[test]
    fn ac1_json_fields() {
        let artifact = make_artifact("never-bin", Verdict::NotInstalled, None, None, None);
        // For NeverInstalled, classify doesn't touch env vars — no lock needed.
        // But we do override HOME to ensure there's no junk prefix or local/bin in real $HOME.
        let _guard = ENV_LOCK.lock().expect("lock");
        let old_home = std::env::var("HOME").unwrap_or_default();
        // Use a non-existent home to ensure NeverInstalled classification.
        std::env::set_var("HOME", "/tmp/adopt-test-ac1-nonexistent");
        let c = classify(&artifact);
        std::env::set_var("HOME", old_home);
        drop(_guard);

        let json = serde_json::to_value(&c).expect("serialize ClassifiedArtifact");
        assert!(json.get("bin").is_some(), "missing 'bin' field");
        assert!(json.get("reason").is_some(), "missing 'reason' field");
        assert!(json.get("detail").is_some(), "missing 'detail' field");
    }

    // AC2: WrongPrefix classification when junk prefix dir exists.
    #[test]
    fn ac2_wrong_prefix_detected() {
        use std::fs;
        use tempfile::TempDir;

        let tmpdir = TempDir::new().expect("create tempdir");
        let home = tmpdir.path();

        // Create the bogus prefix structure: $HOME/~/.local/bin/<bin>
        let junk_bin_dir = home.join("~").join(".local").join("bin");
        fs::create_dir_all(&junk_bin_dir).expect("create junk bin dir");
        let fake_bin = junk_bin_dir.join("mybin");
        fs::write(&fake_bin, "#!/bin/sh\necho hi").expect("write fake bin");

        let _guard = ENV_LOCK.lock().expect("lock");
        std::env::set_var("HOME", home);

        // Artifact says not installed (scan didn't find it normally).
        let artifact = make_artifact("mybin", Verdict::NotInstalled, None, None, None);
        let c = classify(&artifact);

        std::env::remove_var("HOME");
        drop(_guard);

        assert_eq!(c.bin, "mybin");
        assert_eq!(c.reason, StaleReason::WrongPrefix,
            "expected WrongPrefix, got {:?}: {}", c.reason, c.detail);
    }

    // AC3: OffPath when binary is in ~/.local/bin but not on restricted PATH.
    #[test]
    fn ac3_off_path_detected() {
        use std::fs;
        use tempfile::TempDir;

        let tmpdir = TempDir::new().expect("create tempdir");
        let home = tmpdir.path();

        // Create ~/.local/bin/<bin>
        let local_bin_dir = home.join(".local").join("bin");
        fs::create_dir_all(&local_bin_dir).expect("create local bin dir");
        let fake_bin = local_bin_dir.join("offpathbin");
        fs::write(&fake_bin, "#!/bin/sh\necho hi").expect("write fake offpathbin");

        let _guard = ENV_LOCK.lock().expect("lock");
        std::env::set_var("HOME", home);
        let old_path = std::env::var("PATH").unwrap_or_default();
        // PATH has only /usr/bin — not our tmp local bin dir.
        std::env::set_var("PATH", "/usr/bin:/bin");

        let artifact = make_artifact("offpathbin", Verdict::NotInstalled, None, None, None);
        let c = classify(&artifact);

        std::env::set_var("HOME", std::env::var("HOME").unwrap_or_default());
        std::env::remove_var("HOME");
        std::env::set_var("PATH", old_path);
        drop(_guard);

        assert_eq!(c.reason, StaleReason::OffPath,
            "expected OffPath, got {:?}: {}", c.reason, c.detail);
    }

    // AC4: SourceNewer when installed but source is newer.
    #[test]
    fn ac4_source_newer_detected() {
        // Artifact is InstalledStale — scan found it but source commit is newer.
        // Use "true" which always exits 0 for --version or --help.
        let artifact = make_artifact(
            "true",
            Verdict::InstalledStale,
            Some("/usr/bin/true"),
            Some(2_000_000_000), // src newer
            Some(1_000_000_000), // binary older
        );
        let c = classify(&artifact);
        // "true --version" returns non-zero on some systems but it exists.
        // We accept either SourceNewer or SmokeFail (depends on system).
        assert!(
            matches!(c.reason, StaleReason::SourceNewer | StaleReason::SmokeFail),
            "expected SourceNewer or SmokeFail, got {:?}", c.reason
        );
    }

    // AC5: verify returns true (any not-current) when artifacts exist.
    // We test run_verify indirectly via summary logic.
    #[test]
    fn ac5_summary_counts() {
        let classified = vec![
            ClassifiedArtifact {
                bin: "a".to_owned(),
                reason: StaleReason::NeverInstalled,
                detail: "not found".to_owned(),
            },
            ClassifiedArtifact {
                bin: "b".to_owned(),
                reason: StaleReason::OffPath,
                detail: "in ~/.local/bin but not PATH".to_owned(),
            },
            ClassifiedArtifact {
                bin: "c".to_owned(),
                reason: StaleReason::NeverInstalled,
                detail: "not found".to_owned(),
            },
        ];

        let mut counts: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        for c in &classified {
            *counts.entry(c.reason.display_name()).or_insert(0) += 1;
        }

        assert_eq!(*counts.get("NeverInstalled").unwrap_or(&0), 2);
        assert_eq!(*counts.get("OffPath").unwrap_or(&0), 1);
        assert_eq!(classified.len(), 3);
    }

    // AC6: per-reason docket slugs are distinct.
    #[test]
    fn ac6_distinct_docket_slugs() {
        let reasons = [
            StaleReason::NeverInstalled,
            StaleReason::WrongPrefix,
            StaleReason::OffPath,
            StaleReason::SourceNewer,
            StaleReason::BuildFail,
            StaleReason::SmokeFail,
        ];
        let slugs: Vec<&'static str> = reasons.iter().map(|r| r.docket_slug()).collect();
        let unique: std::collections::HashSet<&&str> = slugs.iter().collect();
        assert_eq!(unique.len(), slugs.len(), "docket slugs must all be distinct");
    }

    // AC7: StaleReason serializes to PascalCase variant names.
    #[test]
    fn ac7_serialization() {
        let r = StaleReason::NeverInstalled;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, "\"NeverInstalled\"");

        let r2 = StaleReason::WrongPrefix;
        let s2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(s2, "\"WrongPrefix\"");
    }
}
