//! Install markers — per-artifact state used to skip unchanged reinstalls.
//!
//! After a successful `cargo install`, a JSON marker is written to
//! `$XDG_STATE_HOME/adopt/markers/<bin>.json`.  On the next `adopt apply`
//! the marker is compared against the current source fingerprint; if they
//! match *and* the binary is still invokable, the install is skipped.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

/// A source fingerprint identifies a specific build state of a repo.
///
/// Two identical fingerprints mean the sources have not changed since the
/// last recorded install, so cargo reinstall can safely be skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprint(pub String);

impl std::fmt::Display for SourceFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The marker written after a verified successful install.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallMarker {
    /// Binary name (without path).
    pub bin: String,
    /// Absolute path to the repo.
    pub repo_path: String,
    /// Source fingerprint at install time.
    pub source_fingerprint: SourceFingerprint,
    /// Unix timestamp (seconds since epoch) when the marker was written.
    pub installed_at: u64,
    /// How this marker was created: `"install"` for a real `adopt apply`,
    /// `"reconcile-seed"` for a clock-seeded marker from `adopt reconcile`.
    /// Defaults to `"install"` when the field is absent (legacy markers).
    #[serde(default = "default_origin")]
    pub origin: String,
}

fn default_origin() -> String {
    "install".to_owned()
}

// ── Fingerprint computation ───────────────────────────────────────────────────

/// Computes the source fingerprint for a repo at `repo_path`.
///
/// Strategy:
/// - If the working tree is clean (no uncommitted changes), use the git HEAD
///   commit hash.
/// - Otherwise, return a string like `"dirty:<max_mtime_secs>"` where
///   `max_mtime_secs` is the newest mtime across `src/**`, `Cargo.toml`, and
///   `Cargo.lock`.  A dirty worktree is always considered changed (never skipped).
///
/// # Errors
///
/// Returns an error only on I/O or git command failures that prevent computing
/// any fingerprint.
pub fn compute_fingerprint(repo_path: &Path) -> Result<SourceFingerprint> {
    // Try git status --porcelain to detect dirty state.
    let status_out = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "status", "--porcelain"])
        .output();

    let is_dirty = match status_out {
        Ok(out) if out.status.success() => !out.stdout.is_empty(),
        // If git is not available or not a repo, treat as dirty.
        _ => true,
    };

    if !is_dirty {
        // Clean: use HEAD commit hash.
        let head_out = std::process::Command::new("git")
            .args(["-C", &repo_path.to_string_lossy(), "rev-parse", "HEAD"])
            .output()
            .context("failed to run git rev-parse HEAD")?;
        if head_out.status.success() {
            let commit = String::from_utf8_lossy(&head_out.stdout)
                .trim()
                .to_owned();
            if !commit.is_empty() {
                return Ok(SourceFingerprint(commit));
            }
        }
    }

    // Dirty (or git unavailable): use max mtime across src/**, Cargo.toml, Cargo.lock.
    let max_mtime = max_src_mtime(repo_path)?;
    Ok(SourceFingerprint(format!("dirty:{max_mtime}")))
}

/// Returns the maximum mtime (seconds since epoch) across `src/**`, `Cargo.toml`,
/// and `Cargo.lock` under `repo_path`.
///
/// Returns 0 if no tracked files exist.
///
/// # Errors
///
/// Returns an error on I/O failures.
fn max_src_mtime(repo_path: &Path) -> Result<u64> {
    let mut max: u64 = 0;

    // Probe Cargo.toml and Cargo.lock directly.
    for filename in &["Cargo.toml", "Cargo.lock"] {
        let p = repo_path.join(filename);
        if let Ok(meta) = std::fs::metadata(&p) {
            max = max.max(mtime_secs(&meta));
        }
    }

    // Walk src/.
    let src_dir = repo_path.join("src");
    if src_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&src_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if let Ok(meta) = entry.metadata() {
                    max = max.max(mtime_secs(&meta));
                }
            }
        }
    }

    Ok(max)
}

/// Extracts seconds-since-epoch from file metadata.
fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Marker path ───────────────────────────────────────────────────────────────

/// Returns the marker file path for a binary name.
///
/// Respects `$XDG_STATE_HOME` (defaults to `~/.local/state`).
///
/// # Errors
///
/// Returns an error if `$HOME` is unset and `$XDG_STATE_HOME` is also unset.
pub fn marker_path(bin: &str) -> Result<PathBuf> {
    let state_home = if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            PathBuf::from(xdg)
        } else {
            default_state_home()?
        }
    } else {
        default_state_home()?
    };
    Ok(state_home.join("adopt").join("markers").join(format!("{bin}.json")))
}

fn default_state_home() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .context("$HOME is unset; cannot determine XDG_STATE_HOME")?;
    Ok(PathBuf::from(home).join(".local").join("state"))
}

// ── Read / write ──────────────────────────────────────────────────────────────

/// Reads an existing marker for `bin`, returning `None` if it does not exist.
///
/// # Errors
///
/// Returns an error on I/O failures or JSON parse errors on an existing file.
pub fn read_marker(bin: &str) -> Result<Option<InstallMarker>> {
    let path = marker_path(bin)?;
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("reading marker {}", path.display()))?;
    let marker: InstallMarker = serde_json::from_str(&data)
        .with_context(|| format!("parsing marker {}", path.display()))?;
    Ok(Some(marker))
}

/// Writes (creates or overwrites) the marker for `bin` after a successful install.
///
/// `origin` records how the marker was created (`"install"` for a real reinstall,
/// `"reconcile-seed"` for a clock-seeded marker from `adopt reconcile`).
///
/// # Errors
///
/// Returns an error on I/O failures.
pub fn write_marker(
    bin: &str,
    repo_path: &str,
    fingerprint: &SourceFingerprint,
    origin: &str,
) -> Result<()> {
    let path = marker_path(bin)?;
    // Ensure the directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating marker dir {}", parent.display()))?;
    }
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let marker = InstallMarker {
        bin: bin.to_owned(),
        repo_path: repo_path.to_owned(),
        source_fingerprint: fingerprint.clone(),
        installed_at: now_secs,
        origin: origin.to_owned(),
    };
    let json = serde_json::to_string_pretty(&marker)
        .context("serialising install marker")?;
    std::fs::write(&path, json)
        .with_context(|| format!("writing marker {}", path.display()))?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Global mutex so env-mutating tests never run concurrently.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` while holding the env lock and with `XDG_STATE_HOME` set to `tmp`.
    fn with_state_home<F: FnOnce(&TempDir)>(f: F) {
        let tmp = TempDir::new().unwrap();
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        f(&tmp);
        std::env::remove_var("XDG_STATE_HOME");
    }

    #[test]
    fn marker_roundtrip() {
        with_state_home(|_tmp| {
            let fp = SourceFingerprint("abc123".to_owned());
            write_marker("mybin", "/home/joe/wintermute/mybin", &fp, "install").unwrap();
            let marker = read_marker("mybin").unwrap().expect("marker should exist");
            assert_eq!(marker.bin, "mybin");
            assert_eq!(marker.repo_path, "/home/joe/wintermute/mybin");
            assert_eq!(marker.source_fingerprint, fp);
            assert!(marker.installed_at > 0);
        });
    }

    #[test]
    fn read_marker_missing_returns_none() {
        with_state_home(|_tmp| {
            let result = read_marker("no-such-bin").unwrap();
            assert!(result.is_none());
        });
    }

    /// AC7: marker_path respects $XDG_STATE_HOME.
    #[test]
    fn marker_path_honors_xdg_state_home() {
        with_state_home(|tmp| {
            let p = marker_path("testbin").unwrap();
            assert!(p.starts_with(tmp.path()), "path {p:?} should be under XDG_STATE_HOME");
        });
    }

    #[test]
    fn dirty_fingerprint_never_matches_clean_commit() {
        // A "dirty:..." fingerprint cannot equal a bare commit hash.
        let fp1 = SourceFingerprint("dirty:1700000000".to_owned());
        let fp2 = SourceFingerprint("abcdef1234567890".to_owned());
        assert_ne!(fp1, fp2);
    }
}
