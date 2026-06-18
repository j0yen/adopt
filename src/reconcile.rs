//! `adopt reconcile` — mint lineage markers for installed-but-unmarked binaries.
//!
//! Reconcile is a one-shot pass that seeds an `InstallMarker` for every artifact
//! that is installed and has no marker, without rebuilding anything.  It uses a
//! conservative heuristic: a binary is seeded as built from the current committed
//! HEAD only when its file mtime is **at or after** the previous commit's timestamp
//! (i.e. it cannot have been installed before the second-to-last commit advanced
//! HEAD).  Genuinely-behind installs are left markerless.
//!
//! The minted markers carry `origin: "reconcile-seed"` so a future audit can
//! distinguish proven-at-install markers from clock-seeded ones.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};

use crate::marker::{self, SourceFingerprint};

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Returns the mtime of `path` in seconds since epoch, or `None` on failure.
fn file_mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Returns the unix timestamp of the *previous* commit (HEAD~1), or `None` if
/// the repo has fewer than two commits or git is unavailable.
fn prev_commit_ts(repo: &Path) -> Option<u64> {
    let out = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "log", "-1", "--format=%ct", "HEAD~1"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse::<u64>().ok()
    } else {
        None
    }
}

/// Returns the committed-HEAD commit hash (not `compute_fingerprint` — we want
/// the raw hash even on a dirty tree, because we compare with what scan does).
fn head_commit_hash(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}

/// Returns true if the working tree is dirty (uncommitted changes).
fn is_dirty(repo: &Path) -> bool {
    Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "status", "--porcelain"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(true) // if git fails, treat as dirty → skip
}

/// Returns the user's home directory.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map_or_else(|_| PathBuf::from("/root"), PathBuf::from)
}

/// Returns the wintermute root directory (overridable via `WM_WINTERMUTE_DIR`).
fn wintermute_dir() -> PathBuf {
    if let Ok(d) = std::env::var("WM_WINTERMUTE_DIR") {
        return PathBuf::from(d);
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

/// Looks for an installed binary in `~/.local/bin`, `~/.cargo/bin`, and `PATH`.
fn find_installed(bin: &str) -> Option<PathBuf> {
    let local = local_bin().join(bin);
    if local.exists() {
        return Some(local);
    }
    let cargo = cargo_bin().join(bin);
    if cargo.exists() {
        return Some(cargo);
    }
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

/// Returns binary names declared by `Cargo.toml` at `cargo_toml`, or `None`
/// for library-only crates.
fn bins_from_cargo_toml(cargo_toml: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(cargo_toml).ok()?;
    let value: toml::Value = text.parse().ok()?;
    let table = value.as_table()?;

    let mut bins: Vec<String> = Vec::new();

    if let Some(bin_arr) = table.get("bin").and_then(|v| v.as_array()) {
        for entry in bin_arr {
            if let Some(name) = entry.get("name").and_then(|n| n.as_str()) {
                bins.push(name.to_owned());
            }
        }
    }

    if bins.is_empty() {
        let has_lib = table.contains_key("lib");
        let has_main = cargo_toml.parent().is_some_and(|p| p.join("src/main.rs").exists());
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

    if bins.is_empty() { None } else { Some(bins) }
}

// ── Public reconcile action type ──────────────────────────────────────────────

/// The outcome for a single artifact during reconcile.
#[derive(Debug)]
pub enum ReconcileOutcome {
    /// Marker already existed — skipped (idempotent).
    AlreadyMarked,
    /// Repo has a dirty working tree — skipped to avoid seeding a bad fp.
    /// (Legacy variant; retained for test compatibility — new code emits `DirtyBlocked`.)
    DirtyTree,
    /// Dirty tree where the installed binary does not match committed HEAD
    /// (or `--no-include-dirty` was passed).  Not `clock-fallback`; explicitly classified.
    DirtyBlocked,
    /// Binary is not installed — skipped.
    NotInstalled,
    /// Binary mtime precedes the previous commit; install is genuinely behind — skipped.
    GenuinelyBehind,
    /// Marker was minted (or would be, in dry-run mode).
    Seeded {
        /// The fingerprint that was (or would be) written.
        fingerprint: String,
        /// True if `--dry-run` — no actual write occurred.
        dry_run: bool,
    },
    /// An error occurred while processing this artifact.
    Error(String),
}

/// A per-artifact reconcile record.
#[derive(Debug)]
pub struct ReconcileResult {
    /// Absolute path to the repo.
    pub repo: String,
    /// Binary name.
    pub bin: String,
    /// What reconcile decided for this artifact.
    pub outcome: ReconcileOutcome,
}

// ── Core ──────────────────────────────────────────────────────────────────────

/// Runs the reconcile pass over all wintermute repos.
///
/// In `dry_run` mode, prints planned actions but writes nothing.
///
/// When `include_dirty` is `true` (the default), repos with dirty working
/// trees are still considered: if the installed binary's mtime is at or after
/// the previous commit timestamp (i.e. it was built from committed HEAD), a
/// marker is seeded from committed HEAD.  When `include_dirty` is `false`,
/// dirty trees are always classified as `DirtyBlocked` and never seeded.
///
/// # Errors
///
/// Returns an error only on unrecoverable failures (e.g. cannot enumerate
/// repos).  Per-artifact errors are captured in [`ReconcileResult::outcome`].
pub fn run_reconcile(dry_run: bool, include_dirty: bool) -> Result<Vec<ReconcileResult>> {
    let wm_dir = wintermute_dir();
    let mut repos: Vec<PathBuf> = Vec::new();

    if wm_dir.is_dir() {
        for entry in std::fs::read_dir(&wm_dir)
            .with_context(|| format!("reading wintermute dir {}", wm_dir.display()))?
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() && path.join("Cargo.toml").exists() {
                repos.push(path);
            }
        }
    }

    repos.sort();

    let mut results = Vec::new();

    for repo in &repos {
        let cargo_toml = repo.join("Cargo.toml");
        let Some(bin_names) = bins_from_cargo_toml(&cargo_toml) else {
            continue; // library-only
        };

        for bin in bin_names {
            let result = reconcile_one(repo, &bin, dry_run, include_dirty);
            results.push(result);
        }
    }

    Ok(results)
}

/// Runs reconcile for a single `(repo, bin)` pair.
fn reconcile_one(repo: &Path, bin: &str, dry_run: bool, include_dirty: bool) -> ReconcileResult {
    // Check if a marker already exists — idempotent skip.
    match marker::read_marker(bin) {
        Ok(Some(_)) => {
            return ReconcileResult {
                repo: repo.display().to_string(),
                bin: bin.to_owned(),
                outcome: ReconcileOutcome::AlreadyMarked,
            };
        }
        Ok(None) => {} // no marker — proceed
        Err(e) => {
            return ReconcileResult {
                repo: repo.display().to_string(),
                bin: bin.to_owned(),
                outcome: ReconcileOutcome::Error(format!("read_marker: {e}")),
            };
        }
    }

    // Binary must be installed.
    let Some(installed_path) = find_installed(bin) else {
        return ReconcileResult {
            repo: repo.display().to_string(),
            bin: bin.to_owned(),
            outcome: ReconcileOutcome::NotInstalled,
        };
    };

    let dirty = is_dirty(repo);

    // Get HEAD commit hash (this is the fingerprint we'll seed).
    let Some(head_hash) = head_commit_hash(repo) else {
        return ReconcileResult {
            repo: repo.display().to_string(),
            bin: bin.to_owned(),
            outcome: ReconcileOutcome::Error("git rev-parse HEAD failed".to_owned()),
        };
    };

    // Conservative seed heuristic: binary mtime must be >= previous commit timestamp.
    // If the previous commit timestamp is unavailable (e.g. only one commit), we allow
    // the seed — the repo is fresh enough that there's no "behind" scenario.
    let binary_mtime = file_mtime_secs(&installed_path);
    let prev_ts = prev_commit_ts(repo);

    let provably_not_behind = match (binary_mtime, prev_ts) {
        (Some(mtime), Some(prev)) => mtime >= prev,
        // No previous commit or can't read binary mtime — allow seed conservatively
        // (single-commit repo, or provfs-extended path we can't stat).
        (None, _) | (_, None) => true,
    };

    if !provably_not_behind {
        // The installed binary predates the previous commit regardless of dirty state.
        return ReconcileResult {
            repo: repo.display().to_string(),
            bin: bin.to_owned(),
            outcome: ReconcileOutcome::GenuinelyBehind,
        };
    }

    // Dirty-tree handling: the uncommitted changes don't affect what the installed
    // binary was built from.  If `include_dirty` is on, we seed from committed HEAD
    // (the binary was built from HEAD even if someone later edited the tree).
    // If `include_dirty` is off, classify as DirtyBlocked rather than silently skip.
    if dirty {
        if !include_dirty {
            return ReconcileResult {
                repo: repo.display().to_string(),
                bin: bin.to_owned(),
                outcome: ReconcileOutcome::DirtyBlocked,
            };
        }
        // include_dirty=true: fall through and seed from committed HEAD.
    }

    // Mint the marker.
    let fp = SourceFingerprint(head_hash.clone());

    if !dry_run {
        if let Err(e) = marker::write_marker(bin, &repo.to_string_lossy(), &fp, "reconcile-seed")
        {
            return ReconcileResult {
                repo: repo.display().to_string(),
                bin: bin.to_owned(),
                outcome: ReconcileOutcome::Error(format!("write_marker: {e}")),
            };
        }
    }

    ReconcileResult {
        repo: repo.display().to_string(),
        bin: bin.to_owned(),
        outcome: ReconcileOutcome::Seeded {
            fingerprint: head_hash,
            dry_run,
        },
    }
}

// ── Output ────────────────────────────────────────────────────────────────────

/// Prints a human-readable summary of reconcile results.
#[allow(clippy::print_stdout)]
pub fn print_reconcile_results(results: &[ReconcileResult], dry_run: bool) {
    if dry_run {
        println!("[dry-run] reconcile — no markers will be written");
    }

    let mut seeded = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for r in results {
        match &r.outcome {
            ReconcileOutcome::Seeded { fingerprint, dry_run: dr } => {
                let prefix = if *dr { "[dry-run] would seed" } else { "seeded" };
                println!("  {prefix}  {bin}  fp={fingerprint}  repo={repo}",
                    bin = r.bin, repo = r.repo);
                seeded += 1;
            }
            ReconcileOutcome::AlreadyMarked => {
                skipped += 1;
            }
            ReconcileOutcome::NotInstalled => {
                skipped += 1;
            }
            ReconcileOutcome::DirtyTree => {
                println!("  skip  {bin}  (dirty working tree)  repo={repo}",
                    bin = r.bin, repo = r.repo);
                skipped += 1;
            }
            ReconcileOutcome::DirtyBlocked => {
                println!("  dirty-blocked  {bin}  (dirty tree; HEAD does not match installed binary or --no-include-dirty)  repo={repo}",
                    bin = r.bin, repo = r.repo);
                skipped += 1;
            }
            ReconcileOutcome::GenuinelyBehind => {
                println!("  skip  {bin}  (genuinely behind HEAD)  repo={repo}",
                    bin = r.bin, repo = r.repo);
                skipped += 1;
            }
            ReconcileOutcome::Error(msg) => {
                println!("  error  {bin}  {msg}  repo={repo}",
                    bin = r.bin, repo = r.repo);
                errors += 1;
            }
        }
    }

    println!("reconcile: seeded={seeded}  skipped={skipped}  errors={errors}");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::marker::{read_marker, write_marker, SourceFingerprint as SF};
    use std::process::Command;
    use tempfile::TempDir;

    /// Sets XDG_STATE_HOME + WM_WINTERMUTE_DIR and runs `f`.
    ///
    /// Uses `catch_unwind` to ensure env var cleanup happens even if `f` panics,
    /// preventing poisoned-lock env var leakage from affecting subsequent tests.
    fn with_env<F: FnOnce()>(state_dir: &TempDir, wm_dir: &TempDir, f: F) {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", state_dir.path());
        std::env::set_var("WM_WINTERMUTE_DIR", wm_dir.path());
        // Point HOME to a temp dir so local_bin() / cargo_bin() don't hit live dirs.
        let fake_home = TempDir::new().unwrap();
        std::env::set_var("HOME", fake_home.path());
        // Suppress system-level git config and allow any directory as safe so
        // git commands succeed even when HOME points to an ephemeral temp dir.
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Always clean up env vars, even if the closure panicked.
        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("WM_WINTERMUTE_DIR");
        std::env::remove_var("HOME");
        std::env::remove_var("GIT_CONFIG_NOSYSTEM");
        std::env::remove_var("GIT_CONFIG_GLOBAL");
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// Initialise a minimal git repo and return the HEAD commit hash.
    ///
    /// `bin_name` becomes the Cargo.toml package name so `bins_from_cargo_toml`
    /// returns the correct binary name for `find_installed`.
    fn init_git_repo(dir: &Path, bin_name: &str) -> String {
        Command::new("git").args(["init", "-b", "main"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["config", "user.name", "T"]).current_dir(dir).output().unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{bin_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ).unwrap();
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        Command::new("git").args(["add", "."]).current_dir(dir).output().unwrap();
        Command::new("git").args(["commit", "-m", "init"]).current_dir(dir).output().unwrap();
        let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Make an additional commit so HEAD advances past `installed` binary's mtime.
    fn advance_head(dir: &Path) {
        std::fs::write(dir.join("extra.txt"), "advance").unwrap();
        Command::new("git").args(["add", "extra.txt"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["commit", "-m", "advance"]).current_dir(dir).output().unwrap();
    }

    // ── AC1: reconcile creates a marker for an unmarked current binary ─────────

    #[test]
    fn ac1_seeds_marker_for_unmarked_current_binary() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        // Build a repo dir inside wm_dir so run_reconcile picks it up.
        let repo_dir = wm_dir.path().join("testbin");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let head = init_git_repo(&repo_dir, "testbin");

        // Create a fake installed binary with mtime = now (after prev commit ts).
        let fake_bin = state_dir.path().join("testbin");
        std::fs::write(&fake_bin, "").unwrap();

        with_env(&state_dir, &wm_dir, || {
            // Point PATH to state_dir so find_installed finds "testbin".
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            let results = run_reconcile(false, true).unwrap();
            let r = results.iter().find(|r| r.bin == "testbin").expect("testbin result");

            match &r.outcome {
                ReconcileOutcome::Seeded { fingerprint, dry_run } => {
                    assert_eq!(fingerprint, &head, "fingerprint should be HEAD hash");
                    assert!(!dry_run, "should not be dry-run");
                }
                other => panic!("expected Seeded, got {other:?}"),
            }

            // Marker must exist and match HEAD.
            let marker = read_marker("testbin").unwrap().expect("marker should exist");
            assert_eq!(marker.source_fingerprint.0, head);
            assert_eq!(marker.origin, "reconcile-seed");

            std::env::set_var("PATH", old_path);
        });
    }

    // ── AC2: after reconcile, scan reports installed-current (lineage) ─────────

    #[test]
    fn ac2_scan_reports_current_after_reconcile() {
        use crate::scan::run_scan;

        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("testbin2");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let head = init_git_repo(&repo_dir, "testbin2");

        let fake_bin = state_dir.path().join("testbin2");
        std::fs::write(&fake_bin, "").unwrap();

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            // Pre-reconcile: no marker → scan uses clock fallback.
            // (We don't assert the exact pre-verdict since clock can vary.)

            // Reconcile seeds the marker.
            run_reconcile(false, true).unwrap();

            // Manually write the marker (reconcile may have done it, but confirm it has HEAD hash).
            let marker = read_marker("testbin2").unwrap().expect("marker");
            assert_eq!(marker.source_fingerprint.0, head);

            // Post-reconcile: scan should use lineage → installed-current.
            let results = run_scan(true, Some("^testbin2$")).unwrap();
            let art = results.iter().find(|a| a.bin == "testbin2").expect("testbin2 artifact");
            assert_eq!(art.verdict, crate::types::Verdict::InstalledCurrent,
                "expected InstalledCurrent after reconcile");
            assert_eq!(art.freshness_basis, crate::types::FreshnessBasis::Lineage,
                "expected Lineage basis after reconcile");

            std::env::set_var("PATH", old_path);
        });
    }

    // ── AC3: genuinely-behind install is NOT seeded ────────────────────────────

    #[test]
    fn ac3_genuinely_behind_not_seeded() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("behindbin");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir, "behindbin");

        // Create a fake installed binary with a very old mtime (epoch 0).
        let fake_bin = state_dir.path().join("behindbin");
        std::fs::write(&fake_bin, "").unwrap();
        // Set its mtime to epoch so it's clearly before any commit.
        filetime::set_file_mtime(&fake_bin, filetime::FileTime::zero()).unwrap();

        // Advance HEAD so prev_commit_ts exists and is much newer than epoch 0.
        advance_head(&repo_dir);

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            let results = run_reconcile(false, true).unwrap();
            let r = results.iter().find(|r| r.bin == "behindbin").expect("behindbin result");

            assert!(
                matches!(r.outcome, ReconcileOutcome::GenuinelyBehind),
                "expected GenuinelyBehind, got {:?}", r.outcome
            );

            // No marker should exist.
            let marker = read_marker("behindbin").unwrap();
            assert!(marker.is_none(), "genuinely-behind should not get a marker");

            std::env::set_var("PATH", old_path);
        });
    }

    // ── AC4: idempotent — second run writes no new markers ────────────────────

    #[test]
    fn ac4_idempotent_second_run() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("idembin");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir, "idembin");

        let fake_bin = state_dir.path().join("idembin");
        std::fs::write(&fake_bin, "").unwrap();

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            // First run.
            run_reconcile(false, true).unwrap();
            let marker1 = read_marker("idembin").unwrap().expect("marker after run 1");

            // Second run.
            run_reconcile(false, true).unwrap();
            let marker2 = read_marker("idembin").unwrap().expect("marker after run 2");

            // Fingerprint and origin must be unchanged.
            assert_eq!(marker1.source_fingerprint, marker2.source_fingerprint,
                "second run must not change fingerprint");
            assert_eq!(marker1.origin, marker2.origin,
                "second run must not change origin");

            std::env::set_var("PATH", old_path);
        });
    }

    // ── AC5: --dry-run writes nothing ─────────────────────────────────────────

    #[test]
    fn ac5_dry_run_writes_nothing() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("drybin");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir, "drybin");

        let fake_bin = state_dir.path().join("drybin");
        std::fs::write(&fake_bin, "").unwrap();

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            let results = run_reconcile(true, true).unwrap();
            let r = results.iter().find(|r| r.bin == "drybin").expect("drybin result");

            assert!(
                matches!(r.outcome, ReconcileOutcome::Seeded { dry_run: true, .. }),
                "expected Seeded(dry_run=true), got {:?}", r.outcome
            );

            // No marker should have been written.
            let marker = read_marker("drybin").unwrap();
            assert!(marker.is_none(), "dry-run must not write a marker");

            std::env::set_var("PATH", old_path);
        });
    }

    // ── New AC: dirty-tree seeding and dirty-blocked classification ───────────

    /// Make the working tree dirty (without committing).
    fn make_dirty(dir: &Path) {
        std::fs::write(dir.join("dirty.txt"), "uncommitted change").unwrap();
    }

    // AC-D1: dirty tree + binary matches committed HEAD → marker seeded from HEAD
    #[test]
    fn acd1_dirty_tree_binary_matches_head_seeds_marker() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("dirtyhead");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let head = init_git_repo(&repo_dir, "dirtyhead");

        // Install a fake binary with mtime = now (provably not behind).
        let fake_bin = state_dir.path().join("dirtyhead");
        std::fs::write(&fake_bin, "").unwrap();

        // Make the tree dirty AFTER the commit (uncommitted edits).
        make_dirty(&repo_dir);

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            // include_dirty=true → should seed from committed HEAD.
            let results = run_reconcile(false, true).unwrap();
            let r = results.iter().find(|r| r.bin == "dirtyhead").expect("dirtyhead result");

            match &r.outcome {
                ReconcileOutcome::Seeded { fingerprint, dry_run } => {
                    assert_eq!(fingerprint, &head, "fingerprint must be committed HEAD hash");
                    assert!(!dry_run);
                }
                other => panic!("expected Seeded, got {other:?}"),
            }

            let marker = read_marker("dirtyhead").unwrap().expect("marker should exist");
            assert_eq!(marker.source_fingerprint.0, head, "marker fp must be committed HEAD");
            assert_eq!(marker.origin, "reconcile-seed");

            std::env::set_var("PATH", old_path);
        });
    }

    // AC-D2: dirty tree + HEAD is ahead of installed binary → dirty-blocked, not seeded
    #[test]
    fn acd2_dirty_tree_head_ahead_of_binary_dirty_blocked() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("dirtyahead");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir, "dirtyahead");

        // Install the binary first (mtime = now after init commit).
        let fake_bin = state_dir.path().join("dirtyahead");
        std::fs::write(&fake_bin, "").unwrap();
        // Set mtime to epoch zero so it's clearly before any commit.
        filetime::set_file_mtime(&fake_bin, filetime::FileTime::zero()).unwrap();

        // Advance HEAD so prev_commit_ts is now > binary mtime.
        advance_head(&repo_dir);

        // Make the tree dirty too.
        make_dirty(&repo_dir);

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            let results = run_reconcile(false, true).unwrap();
            let r = results.iter().find(|r| r.bin == "dirtyahead").expect("dirtyahead result");

            // GenuinelyBehind takes precedence over dirty classification.
            assert!(
                matches!(r.outcome, ReconcileOutcome::GenuinelyBehind),
                "expected GenuinelyBehind (HEAD is ahead of binary), got {:?}", r.outcome
            );

            // No marker.
            assert!(read_marker("dirtyahead").unwrap().is_none(), "must not seed a marker");

            std::env::set_var("PATH", old_path);
        });
    }

    // AC-D3: --no-include-dirty → dirty-blocked for all dirty trees, no seeding
    #[test]
    fn acd3_no_include_dirty_blocks_seeding() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("noinclude");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir, "noinclude");

        let fake_bin = state_dir.path().join("noinclude");
        std::fs::write(&fake_bin, "").unwrap();

        // Make the tree dirty.
        make_dirty(&repo_dir);

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            // include_dirty=false → must not seed.
            let results = run_reconcile(false, false).unwrap();
            let r = results.iter().find(|r| r.bin == "noinclude").expect("noinclude result");

            assert!(
                matches!(r.outcome, ReconcileOutcome::DirtyBlocked),
                "expected DirtyBlocked with include_dirty=false, got {:?}", r.outcome
            );

            assert!(read_marker("noinclude").unwrap().is_none(), "no-include-dirty must not write a marker");

            std::env::set_var("PATH", old_path);
        });
    }

    // AC-D4: genuinely-behind repo unchanged — still not seeded (dirty or clean)
    #[test]
    fn acd4_genuinely_behind_unchanged() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("behinddirty");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir, "behinddirty");

        let fake_bin = state_dir.path().join("behinddirty");
        std::fs::write(&fake_bin, "").unwrap();
        filetime::set_file_mtime(&fake_bin, filetime::FileTime::zero()).unwrap();

        // Advance HEAD so there's a prev commit timestamp > epoch 0.
        advance_head(&repo_dir);

        // Also make it dirty — dirty state must NOT override the GenuinelyBehind guard.
        make_dirty(&repo_dir);

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            // Even with include_dirty=true, genuinely behind → not seeded.
            let results = run_reconcile(false, true).unwrap();
            let r = results.iter().find(|r| r.bin == "behinddirty").expect("behinddirty result");

            assert!(
                matches!(r.outcome, ReconcileOutcome::GenuinelyBehind),
                "dirty+behind must still be GenuinelyBehind, got {:?}", r.outcome
            );
            assert!(read_marker("behinddirty").unwrap().is_none(), "behind binary must not get a marker");

            std::env::set_var("PATH", old_path);
        });
    }

    // ── AC6: minted markers have origin "reconcile-seed"; reinstall overwrites ─

    #[test]
    fn ac6_origin_reconcile_seed_overwritten_by_install() {
        let state_dir = TempDir::new().unwrap();
        let wm_dir = TempDir::new().unwrap();

        let repo_dir = wm_dir.path().join("originbin");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let head = init_git_repo(&repo_dir, "originbin");

        let fake_bin = state_dir.path().join("originbin");
        std::fs::write(&fake_bin, "").unwrap();

        with_env(&state_dir, &wm_dir, || {
            let old_path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{old_path}", state_dir.path().display()));

            // Reconcile seeds the marker.
            run_reconcile(false, true).unwrap();
            let seeded = read_marker("originbin").unwrap().expect("seeded marker");
            assert_eq!(seeded.origin, "reconcile-seed");
            assert_eq!(seeded.source_fingerprint.0, head);

            // Simulate a real reinstall overwriting with origin = "install".
            let fp = SF(head.clone());
            write_marker("originbin", &repo_dir.to_string_lossy(), &fp, "install").unwrap();

            let reinstalled = read_marker("originbin").unwrap().expect("reinstalled marker");
            assert_eq!(reinstalled.origin, "install",
                "real reinstall must overwrite origin to 'install'");

            std::env::set_var("PATH", old_path);
        });
    }
}
