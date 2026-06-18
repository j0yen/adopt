//! Core scanning logic: enumerate wintermute repos, derive verdicts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use crate::marker;
use crate::types::{ArtifactResult, FreshnessBasis, Verdict};

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Returns the user's home directory.
///
/// Reads `HOME` at call time so tests that set `env("HOME", ...)` see it.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map_or_else(|_| PathBuf::from("/root"), PathBuf::from)
}

/// Returns the wintermute root directory.
///
/// Can be overridden with `WM_WINTERMUTE_DIR` for testing.
fn wintermute_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("WM_WINTERMUTE_DIR") {
        return PathBuf::from(override_dir);
    }
    home_dir().join("wintermute")
}

/// Returns `~/.local/bin`.
fn local_bin() -> PathBuf {
    home_dir().join(".local/bin")
}

/// Returns `~/.cargo/bin`.
fn cargo_bin() -> PathBuf {
    home_dir().join(".cargo/bin")
}

// ── Binary enumeration ───────────────────────────────────────────────────────

/// Returns the list of binary names declared by a Cargo.toml, or `None` if it's library-only.
///
/// Best-effort: returns `None` on any parse error (caller skips with a note).
fn bins_from_cargo_toml(path: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = text.parse().ok()?;
    let table = value.as_table()?;

    let mut bins: Vec<String> = Vec::new();

    // [[bin]] sections
    if let Some(bin_arr) = table.get("bin").and_then(|v| v.as_array()) {
        for entry in bin_arr {
            if let Some(name) = entry.get("name").and_then(|n| n.as_str()) {
                bins.push(name.to_owned());
            }
        }
    }

    // If no [[bin]] declared, check for src/main.rs (implicit binary = package name)
    // and no [lib] section.
    if bins.is_empty() {
        let has_lib = table.contains_key("lib");
        let has_main = path.parent().is_some_and(|p| p.join("src/main.rs").exists());
        let default_run = table
            .get("package")
            .and_then(|p| p.get("default-run"))
            .and_then(|d| d.as_str())
            .map(ToOwned::to_owned);

        if let Some(dr) = default_run {
            bins.push(dr);
        } else if !has_lib && has_main {
            let pkg_name = table
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())?;
            bins.push(pkg_name.to_owned());
        }
    }

    if bins.is_empty() {
        None // library-only
    } else {
        Some(bins)
    }
}

// ── Installation detection ───────────────────────────────────────────────────

/// Checks `~/.local/bin/<bin>`, `~/.cargo/bin/<bin>`, and PATH.
///
/// Returns the resolved installed path, if any.
fn find_installed(bin: &str) -> Option<PathBuf> {
    // Check ~/.local/bin first (laptop convention).
    let local = local_bin().join(bin);
    if local.exists() {
        return Some(local);
    }

    // Check ~/.cargo/bin.
    let cargo = cargo_bin().join(bin);
    if cargo.exists() {
        return Some(cargo);
    }

    // Walk PATH (not using `which` to avoid a fork; use std).
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let candidate = PathBuf::from(dir).join(bin);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Returns the mtime of a file as seconds-since-epoch, degrading gracefully.
fn mtime_ts(path: &Path) -> Option<i64> {
    // Try provfs xattr first (user.prov.ts), degrade to mtime.
    let xattr_ts = xattr::get(path, "user.prov.ts")
        .ok()
        .flatten()
        .and_then(|v| {
            let s = String::from_utf8(v).ok()?;
            s.trim().parse::<i64>().ok()
        });

    if let Some(ts) = xattr_ts {
        return Some(ts);
    }

    // Fall back to mtime.
    let meta = std::fs::metadata(path).ok()?;
    let secs = meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())?;
    // Safe: timestamps through year 292_471_208_677 fit in i64.
    i64::try_from(secs).ok()
}

/// Returns the unix timestamp of the newest `src/` commit in the repo.
fn newest_src_commit_ts(repo: &Path) -> Option<i64> {
    let out = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "log", "-1", "--format=%ct", "--", "src/"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse::<i64>().ok()
    } else {
        None
    }
}

// ── Daemon detection ─────────────────────────────────────────────────────────

/// Returns true if there is a systemd user unit whose `ExecStart` references `bin`.
fn is_daemon_bin(bin: &str) -> bool {
    // Search common systemd user unit locations.
    let home = home_dir();
    let unit_dirs = [
        home.join(".config/systemd/user"),
        PathBuf::from("/usr/lib/systemd/user"),
        PathBuf::from("/etc/systemd/user"),
    ];

    for dir in &unit_dirs {
        if !dir.exists() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("service") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if line.trim_start().starts_with("ExecStart") && line.contains(bin) {
                        return true;
                    }
                }
            }
        }
    }

    // Also check if binstale is on PATH and says the bin is a daemon.
    // (binstale operates on PIDs; if the binary isn't running we can't ask it.
    //  But if it IS running, it's definitely a daemon. Check for a running process.)
    if Command::new("pgrep")
        .args(["-x", bin])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return true;
    }

    false
}

// ── Age formatting ────────────────────────────────────────────────────────────

fn format_age(installed_ts: i64, src_ts: i64) -> String {
    let delta = src_ts - installed_ts;
    if delta <= 0 {
        return "current".to_owned();
    }
    let days = delta / 86400;
    let hours = (delta % 86400) / 3600;
    if days > 0 {
        format!("{days}d stale")
    } else {
        format!("{hours}h stale")
    }
}

/// Returns current unix timestamp in seconds.
fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

// ── Manifest reading ──────────────────────────────────────────────────────────

/// Returns repo paths from the build manifest (best-effort).
fn manifest_repo_paths() -> Vec<PathBuf> {
    let manifest_path = home_dir()
        .join(".claude/skills/build/state/manifest.json");
    let Ok(text) = std::fs::read_to_string(&manifest_path) else {
        return Vec::new();
    };
    let Ok(val): Result<Value, _> = serde_json::from_str(&text) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    let prds = match val.get(".prds").or_else(|| val.get("prds")) {
        Some(Value::Array(arr)) => arr.clone(),
        _ => {
            // Try top-level array.
            if let Value::Array(arr) = &val {
                arr.clone()
            } else {
                return paths;
            }
        }
    };

    for entry in &prds {
        if entry.get("status").and_then(|s| s.as_str()) != Some("shipped") {
            continue;
        }
        let output_repo = entry
            .get("output_repo_path")
            .and_then(|v| v.as_str())
            .or_else(|| entry.get("repo").and_then(|v| v.as_str()));
        if let Some(repo) = output_repo {
            let p = PathBuf::from(repo);
            if p.starts_with(wintermute_dir()) && p.is_dir() {
                paths.push(p);
            }
        }
    }
    paths
}

// ── Verdict derivation (split out to keep run_scan under line limit) ──────────

/// Derives the freshness verdict for a single artifact.
///
/// Priority:
/// 1. If an [`InstallMarker`](crate::marker::InstallMarker) exists for `bin`,
///    compare its fingerprint to the current committed-HEAD fingerprint of
///    `repo`. Equal → `InstalledCurrent (Lineage)`, differs →
///    `InstalledStale (Lineage)`.  A dirty working tree is handled
///    correctly: `compute_fingerprint` returns `dirty:…` for dirty trees, so
///    we compare the marker against the *committed* HEAD directly via
///    `git rev-parse HEAD`, ensuring a binary built from the last commit is
///    not falsely reported stale while the tree is dirty.
/// 2. If no marker is present (legacy or unmarked install), fall back to the
///    original timestamp comparison (`installed_ts < src_commit_ts`).
///
/// Returns `(Verdict, FreshnessBasis)`.
fn derive_verdict(
    installed: Option<&PathBuf>,
    installed_ts: Option<i64>,
    src_ts: Option<i64>,
    bin: &str,
    repo: &Path,
) -> (Verdict, FreshnessBasis) {
    if installed.is_none() {
        // Not installed — basis is not meaningful, use clock-fallback.
        return (Verdict::NotInstalled, FreshnessBasis::ClockFallback);
    }

    // Attempt lineage-based verdict via InstallMarker.
    if let Ok(Some(marker_data)) = marker::read_marker(bin) {
        // Compute the committed-HEAD fingerprint for this repo.
        // We call git rev-parse HEAD directly so that a dirty working tree
        // (which would produce "dirty:…" from compute_fingerprint) does not
        // falsely report stale when the binary matches the last commit.
        let committed_head = std::process::Command::new("git")
            .args(["-C", &repo.to_string_lossy(), "rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                let s = String::from_utf8(o.stdout).ok()?;
                let trimmed = s.trim().to_owned();
                if trimmed.is_empty() { None } else { Some(trimmed) }
            });

        if let Some(head_hash) = committed_head {
            let marker_fp = &marker_data.source_fingerprint.0;
            let verdict = if marker_fp == &head_hash {
                Verdict::InstalledCurrent
            } else {
                Verdict::InstalledStale
            };
            return (verdict, FreshnessBasis::Lineage);
        }
        // git unavailable or not a repo — fall through to clock.
    }

    // Clock fallback: original behavior.
    let verdict = if let (Some(its), Some(sts)) = (installed_ts, src_ts) {
        if its < sts {
            Verdict::InstalledStale
        } else {
            Verdict::InstalledCurrent
        }
    } else {
        // Can't compare timestamps → assume current to avoid false stale alarms.
        Verdict::InstalledCurrent
    };
    (verdict, FreshnessBasis::ClockFallback)
}

fn derive_fix_cmd(verdict: &Verdict, is_daemon: bool, repo: &Path) -> String {
    if !verdict.is_actionable() {
        return String::new();
    }
    if is_daemon {
        format!("rollout install {}", repo.display())
    } else {
        format!("cargo install --force --path {} --root {}", repo.display(), home_dir().join(".local").display())
    }
}

fn derive_age_vs_head(
    installed: Option<&PathBuf>,
    installed_ts: Option<i64>,
    src_ts: Option<i64>,
) -> Option<String> {
    match (installed_ts, src_ts) {
        (Some(its), Some(sts)) => Some(format_age(its, sts)),
        (None, Some(sts)) if installed.is_none() => {
            let age = now_ts() - sts;
            Some(format!("{} (never installed)", format_age(0, age)))
        }
        _ => None,
    }
}

// ── Main scan ─────────────────────────────────────────────────────────────────

/// Runs the full adoption scan and returns all results.
///
/// # Errors
/// Returns an error only on truly unrecoverable failures (not per-repo skips).
pub fn run_scan(show_all: bool, match_re: Option<&str>) -> Result<Vec<ArtifactResult>> {
    let filter_re = match_re
        .map(|s| Regex::new(s).with_context(|| format!("invalid --match regex: {s}")))
        .transpose()?;

    let wm_dir = wintermute_dir();
    let mut repos: Vec<PathBuf> = Vec::new();

    // (1) Walk ~/wintermute/*/Cargo.toml
    if wm_dir.is_dir() {
        for entry in std::fs::read_dir(&wm_dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("Cargo.toml").exists() {
                repos.push(path);
            }
        }
    }

    // (2) Manifest additive: repos from manifest that aren't already found.
    for mp in manifest_repo_paths() {
        if !repos.contains(&mp) {
            repos.push(mp);
        }
    }

    repos.sort();

    let mut results: Vec<ArtifactResult> = Vec::new();

    for repo in repos {
        let cargo_toml = repo.join("Cargo.toml");
        // Skip unreadable / malformed Cargo.toml silently.
        let Some(bin_names) = bins_from_cargo_toml(&cargo_toml) else {
            // Library-only — include only when --all.
            if show_all {
                results.push(ArtifactResult {
                    repo: repo.display().to_string(),
                    bin: "(library)".to_owned(),
                    verdict: Verdict::NotABin,
                    installed_path: None,
                    is_daemon: false,
                    source_commit_ts: None,
                    installed_ts: None,
                    fix_cmd: String::new(),
                    age_vs_head: None,
                    freshness_basis: FreshnessBasis::ClockFallback,
                });
            }
            continue;
        };

        let src_ts = newest_src_commit_ts(&repo);

        for bin in bin_names {
            if let Some(re) = &filter_re {
                if !re.is_match(&bin) {
                    continue;
                }
            }

            let installed = find_installed(&bin);
            let installed_path_str = installed.as_ref().map(|p| p.display().to_string());
            let installed_ts = installed.as_ref().and_then(|p| mtime_ts(p));
            let is_daemon = is_daemon_bin(&bin);

            let (verdict, freshness_basis) =
                derive_verdict(installed.as_ref(), installed_ts, src_ts, &bin, &repo);

            // Skip installed-current / not-a-bin unless --all.
            if !show_all && !verdict.is_actionable() {
                continue;
            }

            let fix_cmd = derive_fix_cmd(&verdict, is_daemon, &repo);
            let age_vs_head = derive_age_vs_head(installed.as_ref(), installed_ts, src_ts);

            results.push(ArtifactResult {
                repo: repo.display().to_string(),
                bin: bin.clone(),
                verdict,
                installed_path: installed_path_str,
                is_daemon,
                source_commit_ts: src_ts,
                installed_ts,
                fix_cmd,
                age_vs_head,
                freshness_basis,
            });
        }
    }

    Ok(results)
}

// ── Output formatters ─────────────────────────────────────────────────────────

/// Prints a human-readable table.
#[allow(clippy::print_stdout)]
pub fn print_table(results: &[ArtifactResult]) {
    if results.is_empty() {
        println!("All scanned artifacts are adopted and current.");
        return;
    }

    println!("{:<20} {:<20} {:<18} {:<12} FIX", "BIN", "VERDICT", "AGE-VS-HEAD", "DAEMON");
    let sep = "-".repeat(100_usize);
    println!("{sep}");

    for r in results {
        let verdict_str = serde_json::to_value(&r.verdict)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| format!("{:?}", r.verdict));

        println!(
            "{:<20} {:<20} {:<18} {:<12} {}",
            r.bin,
            verdict_str,
            r.age_vs_head.as_deref().unwrap_or("-"),
            if r.is_daemon { "daemon" } else { "" },
            r.fix_cmd,
        );
    }
}

/// Emits JSON array to stdout.
///
/// # Errors
/// Returns an error if serialization fails.
pub fn print_json(results: &[ArtifactResult]) -> Result<()> {
    #[allow(clippy::print_stdout)]
    {
        println!("{}", serde_json::to_string_pretty(results)?);
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::marker::{write_marker, SourceFingerprint};
    use std::process::Command;

    // ── format_age ────────────────────────────────────────────────────────────

    #[test]
    fn format_age_current() {
        // installed_ts >= src_ts → "current"
        assert_eq!(format_age(1000, 900), "current");
        assert_eq!(format_age(1000, 1000), "current");
    }

    #[test]
    fn format_age_days() {
        // 9 days stale
        let src = 1_000_000;
        let inst = src - 9 * 86400;
        assert_eq!(format_age(inst, src), "9d stale");
    }

    #[test]
    fn format_age_hours() {
        let src = 1_000_000;
        let inst = src - 5 * 3600;
        assert_eq!(format_age(inst, src), "5h stale");
    }

    // ── Helpers for lineage tests ─────────────────────────────────────────────

    /// Initialise a minimal git repo in `dir` and return the HEAD commit hash.
    fn init_git_repo(dir: &std::path::Path) -> String {
        // Init
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .expect("git config name");

        // Add a file and commit so HEAD exists.
        std::fs::write(dir.join("README"), "hello").expect("write README");
        Command::new("git")
            .args(["add", "README"])
            .current_dir(dir)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .expect("git commit");

        // Return HEAD hash.
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse HEAD");
        String::from_utf8(out.stdout).expect("utf8").trim().to_owned()
    }

    // ── AC1: marker fingerprint matches HEAD → InstalledCurrent (Lineage) ────

    /// AC1: Even when `installed_ts < src_commit_ts` (clock says stale),
    /// a marker whose fingerprint equals the committed HEAD returns
    /// `InstalledCurrent` with basis `Lineage`.
    #[test]
    fn ac1_lineage_current_overrides_clock_stale() {
        use tempfile::TempDir;

        let repo_dir = TempDir::new().expect("repo dir");
        let state_dir = TempDir::new().expect("state dir");

        // Set up git repo and get HEAD hash.
        let head_hash = init_git_repo(repo_dir.path());

        // Write a marker whose fingerprint IS the HEAD hash.
        let fp = SourceFingerprint(head_hash.clone());
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());
        write_marker("ac1-testbin", &repo_dir.path().to_string_lossy(), &fp, "install")
            .expect("write_marker");

        // Fake an installed binary path (just needs to be Some).
        let fake_bin = state_dir.path().join("ac1-testbin");
        std::fs::write(&fake_bin, "").expect("write fake bin");

        // installed_ts is LESS than src_ts (clock says stale).
        let installed_ts: Option<i64> = Some(1_000_000);
        let src_ts: Option<i64> = Some(2_000_000);

        let (verdict, basis) = derive_verdict(
            Some(&fake_bin),
            installed_ts,
            src_ts,
            "ac1-testbin",
            repo_dir.path(),
        );

        std::env::remove_var("XDG_STATE_HOME");
        drop(_guard);

        assert_eq!(verdict, Verdict::InstalledCurrent, "expected InstalledCurrent by lineage");
        assert_eq!(basis, crate::types::FreshnessBasis::Lineage, "expected Lineage basis");
    }

    // ── AC2: marker fingerprint differs from HEAD → InstalledStale (Lineage) ─

    /// AC2: A marker with a stale fingerprint (an old commit hash) yields
    /// `InstalledStale` with basis `Lineage`, proving genuine behind.
    #[test]
    fn ac2_lineage_stale_when_fingerprint_differs() {
        use tempfile::TempDir;

        let repo_dir = TempDir::new().expect("repo dir");
        let state_dir = TempDir::new().expect("state dir");

        // Set up git repo and get HEAD hash.
        let head_hash = init_git_repo(repo_dir.path());

        // Write a marker with a DIFFERENT (old) fingerprint.
        let old_fp = SourceFingerprint(format!("old-{head_hash}"));
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());
        write_marker("ac2-testbin", &repo_dir.path().to_string_lossy(), &old_fp, "install")
            .expect("write_marker");

        let fake_bin = state_dir.path().join("ac2-testbin");
        std::fs::write(&fake_bin, "").expect("write fake bin");

        let (verdict, basis) = derive_verdict(
            Some(&fake_bin),
            Some(2_000_000),
            Some(1_000_000), // clock says current, but marker wins
            "ac2-testbin",
            repo_dir.path(),
        );

        std::env::remove_var("XDG_STATE_HOME");
        drop(_guard);

        assert_eq!(verdict, Verdict::InstalledStale, "expected InstalledStale by lineage");
        assert_eq!(basis, crate::types::FreshnessBasis::Lineage, "expected Lineage basis");
    }

    // ── AC3: no marker → byte-for-byte existing clock behavior ───────────────

    /// AC3a: No marker, installed_ts < src_ts → InstalledStale (ClockFallback).
    #[test]
    fn ac3a_no_marker_clock_stale() {
        use tempfile::TempDir;

        let repo_dir = TempDir::new().expect("repo dir");
        let state_dir = TempDir::new().expect("state dir");
        let _ = init_git_repo(repo_dir.path());

        let fake_bin = state_dir.path().join("ac3a-bin");
        std::fs::write(&fake_bin, "").expect("write fake bin");

        // No marker written — XDG_STATE_HOME points to empty dir.
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());

        let (verdict, basis) = derive_verdict(
            Some(&fake_bin),
            Some(1_000_000), // installed_ts < src_ts
            Some(2_000_000),
            "ac3a-bin",
            repo_dir.path(),
        );

        std::env::remove_var("XDG_STATE_HOME");
        drop(_guard);

        assert_eq!(verdict, Verdict::InstalledStale, "clock fallback should say stale");
        assert_eq!(basis, crate::types::FreshnessBasis::ClockFallback);
    }

    /// AC3b: No marker, installed_ts >= src_ts → InstalledCurrent (ClockFallback).
    #[test]
    fn ac3b_no_marker_clock_current() {
        use tempfile::TempDir;

        let repo_dir = TempDir::new().expect("repo dir");
        let state_dir = TempDir::new().expect("state dir");
        let _ = init_git_repo(repo_dir.path());

        let fake_bin = state_dir.path().join("ac3b-bin");
        std::fs::write(&fake_bin, "").expect("write fake bin");

        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());

        let (verdict, basis) = derive_verdict(
            Some(&fake_bin),
            Some(3_000_000), // installed_ts >= src_ts
            Some(2_000_000),
            "ac3b-bin",
            repo_dir.path(),
        );

        std::env::remove_var("XDG_STATE_HOME");
        drop(_guard);

        assert_eq!(verdict, Verdict::InstalledCurrent, "clock fallback should say current");
        assert_eq!(basis, crate::types::FreshnessBasis::ClockFallback);
    }

    // ── AC4: dirty working tree, binary built from last commit → current ──────

    /// AC4: With a dirty working tree (uncommitted changes), a binary whose
    /// marker fingerprint equals the committed HEAD is still `InstalledCurrent`.
    /// The dirty-tree state must not produce a false stale.
    #[test]
    fn ac4_dirty_tree_does_not_falsely_stale() {
        use tempfile::TempDir;

        let repo_dir = TempDir::new().expect("repo dir");
        let state_dir = TempDir::new().expect("state dir");

        // Set up repo and get the committed HEAD hash.
        let head_hash = init_git_repo(repo_dir.path());

        // Make the working tree dirty (untracked / modified file).
        std::fs::write(repo_dir.path().join("dirty.txt"), "uncommitted change")
            .expect("write dirty file");

        // Marker fingerprint = committed HEAD (binary was built from that commit).
        let fp = SourceFingerprint(head_hash.clone());
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());
        write_marker("ac4-testbin", &repo_dir.path().to_string_lossy(), &fp, "install")
            .expect("write_marker");

        let fake_bin = state_dir.path().join("ac4-testbin");
        std::fs::write(&fake_bin, "").expect("write fake bin");

        let (verdict, basis) = derive_verdict(
            Some(&fake_bin),
            Some(1_000_000), // clock says stale
            Some(2_000_000),
            "ac4-testbin",
            repo_dir.path(),
        );

        std::env::remove_var("XDG_STATE_HOME");
        drop(_guard);

        assert_eq!(
            verdict,
            Verdict::InstalledCurrent,
            "dirty tree must not produce false stale; HEAD hash={head_hash}"
        );
        assert_eq!(basis, crate::types::FreshnessBasis::Lineage);
    }

    // ── AC6: write_marker → derive_verdict round-trip ─────────────────────────

    /// AC6: A marker written via the apply path (write_marker) and then read
    /// by derive_verdict produces `InstalledCurrent` with no reinstall.
    /// This confirms the same fingerprint function is used on both sides.
    #[test]
    fn ac6_apply_write_scan_read_roundtrip() {
        use crate::marker::compute_fingerprint;
        use tempfile::TempDir;

        let repo_dir = TempDir::new().expect("repo dir");
        let state_dir = TempDir::new().expect("state dir");
        let _ = init_git_repo(repo_dir.path());

        // Compute fingerprint the same way apply does.
        let fp = compute_fingerprint(repo_dir.path()).expect("compute_fingerprint");

        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());
        write_marker("ac6-testbin", &repo_dir.path().to_string_lossy(), &fp, "install")
            .expect("write_marker");

        let fake_bin = state_dir.path().join("ac6-testbin");
        std::fs::write(&fake_bin, "").expect("write fake bin");

        // Now scan reads the marker — should match and return current.
        let (verdict, basis) = derive_verdict(
            Some(&fake_bin),
            Some(1_000_000),
            Some(2_000_000),
            "ac6-testbin",
            repo_dir.path(),
        );

        std::env::remove_var("XDG_STATE_HOME");
        drop(_guard);

        assert_eq!(verdict, Verdict::InstalledCurrent, "apply→scan roundtrip should be current");
        assert_eq!(basis, crate::types::FreshnessBasis::Lineage);
    }
}
