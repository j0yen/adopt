//! Core scanning logic: enumerate wintermute repos, derive verdicts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use crate::types::{ArtifactResult, Verdict};

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

const fn derive_verdict(
    installed: Option<&PathBuf>,
    installed_ts: Option<i64>,
    src_ts: Option<i64>,
) -> Verdict {
    if installed.is_none() {
        return Verdict::NotInstalled;
    }
    if let (Some(its), Some(sts)) = (installed_ts, src_ts) {
        if its < sts {
            return Verdict::InstalledStale;
        }
        return Verdict::InstalledCurrent;
    }
    // Can't compare timestamps → assume current to avoid false stale alarms.
    Verdict::InstalledCurrent
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

            let verdict = derive_verdict(installed.as_ref(), installed_ts, src_ts);

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
mod tests {
    use super::*;

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
}
