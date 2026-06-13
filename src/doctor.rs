//! `adopt doctor` — detect and optionally clean adopt-created junk under literal-tilde prefixes.
//!
//! The bug: when `$HOME` contains a literal `~` (or `--root ~/.local` is passed unexpanded),
//! `cargo install` creates a tree at `$HOME/~/.local/bin/…`. This module finds those trees,
//! reports them, and (with `--clean`) removes binaries that already have a correctly-installed
//! twin in `~/.local/bin` or `~/.cargo/bin`.

use std::path::{Path, PathBuf};

use anyhow::Result;

// ── Path helpers (mirrors scan.rs, kept local to avoid coupling) ──────────────

/// Returns the user's home directory from `$HOME`.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map_or_else(|_| PathBuf::from("/root"), PathBuf::from)
}

// ── Junk detection ────────────────────────────────────────────────────────────

/// One piece of junk found under a literal-tilde prefix.
#[derive(Debug)]
pub struct JunkEntry {
    /// Absolute path to the junk binary.
    pub junk_path: PathBuf,
    /// Byte size of the junk binary.
    pub size_bytes: u64,
    /// Path of the correctly-installed twin, if any.
    pub twin: Option<PathBuf>,
}

/// Scans `$HOME/~` and sub-directories (up to depth 4) for adopt-created junk.
///
/// A directory is considered adopt-created if it contains a `.crates.toml` sibling
/// confirming `cargo` authored it.
pub(crate) fn find_junk_entries(home: &Path) -> Vec<JunkEntry> {
    // The junk root is the literal `~` directory directly under $HOME.
    let junk_root = home.join("~");
    if !junk_root.exists() {
        return Vec::new();
    }

    // Check for the cargo-created .crates.toml anywhere under junk_root/…/.local/
    // (depth 2 is typical: ~/~/.local/.crates.toml)
    let has_cargo_marker = has_crates_toml(&junk_root);

    if !has_cargo_marker {
        // Not confident this is adopt-authored junk; leave it alone.
        return Vec::new();
    }

    // Collect executable files under junk_root up to maxdepth 4.
    let mut entries: Vec<JunkEntry> = Vec::new();
    collect_executables(&junk_root, 0, 4, home, &mut entries);
    entries
}

/// Recursively collects executable files under `dir`, up to `max_depth`.
/// `home` is used to locate real twin directories (`home/.local/bin`, `home/.cargo/bin`).
fn collect_executables(dir: &Path, depth: usize, max_depth: usize, home: &Path, out: &mut Vec<JunkEntry>) {
    if depth > max_depth {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect_executables(&path, depth + 1, max_depth, home, out);
        } else if meta.is_file() && is_executable(&meta) {
            let bin_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let twin = find_twin_in_home(&bin_name, home);
            out.push(JunkEntry {
                junk_path: path,
                size_bytes: meta.len(),
                twin,
            });
        }
    }
}

/// Returns true if the file has any executable bit set (owner/group/other).
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// Returns a correctly-installed twin if one exists in `~/.local/bin` or `~/.cargo/bin`.
/// `home` overrides the home directory for the lookup (used in tests).
fn find_twin_in_home(bin_name: &str, home: &Path) -> Option<PathBuf> {
    let local = home.join(".local/bin").join(bin_name);
    if local.exists() {
        return Some(local);
    }
    let cargo = home.join(".cargo/bin").join(bin_name);
    if cargo.exists() {
        return Some(cargo);
    }
    None
}

/// Walks `dir` to find any `.crates.toml` file (depth-limited to 3).
fn has_crates_toml(dir: &Path) -> bool {
    find_crates_toml(dir, 0, 3)
}

fn find_crates_toml(dir: &Path, depth: usize, max_depth: usize) -> bool {
    if depth > max_depth {
        return false;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if find_crates_toml(&path, depth + 1, max_depth) {
                return true;
            }
        } else if path.file_name().map(|n| n == ".crates.toml").unwrap_or(false) {
            return true;
        }
    }
    false
}

// ── Doctor run ────────────────────────────────────────────────────────────────

/// Runs `adopt doctor [--clean]`.
///
/// Returns `true` if any debris was found (caller should exit non-zero).
///
/// # Errors
///
/// Returns an error on I/O failures during cleanup.
pub fn run_doctor(clean: bool) -> Result<bool> {
    run_doctor_with_home(&home_dir(), clean)
}

/// Inner implementation; separated so tests can inject an explicit home without
/// mutating `$HOME` (which is not thread-safe).
///
/// # Errors
///
/// Returns an error on I/O failures during cleanup.
#[allow(clippy::print_stdout)]
pub(crate) fn run_doctor_with_home(home: &Path, clean: bool) -> Result<bool> {
    let junk_root = home.join("~");
    let entries = find_junk_entries(home);

    if entries.is_empty() {
        println!("adopt doctor: no junk debris found under `{}`", junk_root.display());
        return Ok(false);
    }

    println!(
        "adopt doctor: found {} junk {} under `{}`",
        entries.len(),
        if entries.len() == 1 { "entry" } else { "entries" },
        junk_root.display()
    );
    println!();
    println!("{:<60} {:<8} TWIN", "JUNK_PATH", "SIZE");
    println!("{}", "-".repeat(90));

    let mut any_debris = false;
    let mut removed_paths: Vec<PathBuf> = Vec::new();

    for entry in &entries {
        any_debris = true;
        let twin_str = entry
            .twin
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_owned());
        println!(
            "{:<60} {:>7}B {}",
            entry.junk_path.display(),
            entry.size_bytes,
            twin_str
        );

        if clean {
            if entry.twin.is_some() {
                // Safe to remove: a correctly-installed twin exists.
                match std::fs::remove_file(&entry.junk_path) {
                    Ok(()) => {
                        println!("  → removed: {}", entry.junk_path.display());
                        removed_paths.push(entry.junk_path.clone());
                    }
                    Err(e) => {
                        println!("  → WARN: could not remove {}: {e}", entry.junk_path.display());
                    }
                }
            } else {
                println!(
                    "  → kept (no twin): {} — remove manually if safe",
                    entry.junk_path.display()
                );
            }
        }
    }

    // After removals, attempt to prune empty directories inside the junk root
    // (never recursively rm a dir that still has files).
    if clean && !removed_paths.is_empty() {
        prune_empty_dirs(&junk_root);
    }

    Ok(any_debris)
}

/// Walks `dir` bottom-up and removes directories that are now empty.
/// Never removes `dir` itself.
fn prune_empty_dirs(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            prune_empty_dirs(&path);
            // Attempt removal; silently ignore if non-empty (OS will reject).
            let _ = std::fs::remove_dir(&path);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a fake junk tree:
    ///   $TMPHOME/~/.local/bin/<name>  (executable)
    ///   $TMPHOME/~/.local/.crates.toml
    fn make_junk_tree(home: &Path, bin_name: &str) -> PathBuf {
        let bin_dir = home.join("~/.local/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let crates_toml = home.join("~/.local/.crates.toml");
        fs::write(&crates_toml, "[v1]\n").unwrap();
        let bin_path = bin_dir.join(bin_name);
        fs::write(&bin_path, "#!/bin/sh\necho fake").unwrap();
        // Make executable.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();
        bin_path
    }

    /// Build a "real" twin at the given location.
    fn make_twin(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "#!/bin/sh\necho real").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // AC1 / AC3 — validate_root literal tilde & outside $HOME
    // These tests don't mutate $HOME; they use the real value which is stable.
    #[test]
    fn validate_root_rejects_tilde() {
        // AC1: literal tilde component
        let result = crate::apply::validate_root("~/.local");
        assert!(result.is_err(), "expected Err for ~/ prefix, got {:?}", result);
    }

    #[test]
    fn validate_root_accepts_absolute_under_home() {
        // AC1 (positive): absolute path under real $HOME
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return; // skip in environments without $HOME
        }
        // Build a path that is definitely under $HOME — use $HOME itself.
        // We don't need to canonicalize; /home/jsy canonicalises to /home/jsy.
        let root = format!("{home}/.local");
        let result = crate::apply::validate_root(&root);
        assert!(result.is_ok(), "expected Ok for absolute path under $HOME `{home}`, got {:?}", result);
    }

    #[test]
    fn validate_root_rejects_outside_home() {
        // AC3: path outside $HOME — /tmp is not under $HOME on any sane system.
        let result = crate::apply::validate_root("/tmp/evil");
        assert!(result.is_err(), "expected Err for /tmp/evil, got {:?}", result);
    }

    // AC2 — BadPrefix outcome when fix_cmd carries --root ~/.local
    #[test]
    fn bad_prefix_outcome_never_spawns() {
        use crate::apply::ApplyOutcome;
        // Simulate what run_apply does: parse --root token from fix_cmd and run
        // validate_root on it. A literal-tilde root must yield BadPrefix and the
        // cargo command must never be reached.
        let fix_cmd = "cargo install --force --path /tmp/fake --root ~/.local";
        let argv: Vec<&str> = fix_cmd.split_whitespace().collect();
        let root_idx = argv.iter().position(|a| *a == "--root");
        let root_val = root_idx.and_then(|i| argv.get(i + 1)).copied().unwrap_or("");
        let outcome = match crate::apply::validate_root(root_val) {
            Err(reason) => ApplyOutcome::BadPrefix { resolved: reason },
            Ok(_) => ApplyOutcome::InstalledOk,
        };
        assert!(
            matches!(outcome, ApplyOutcome::BadPrefix { .. }),
            "expected BadPrefix, got {:?}",
            outcome
        );
    }

    // AC4 — adopt doctor detects junk tree, exits non-zero
    // Uses run_doctor_with_home to avoid $HOME mutation.
    #[test]
    fn doctor_detects_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        make_junk_tree(home, "fake-bin");

        let result = run_doctor_with_home(home, false).unwrap();
        assert!(result, "expected doctor to report debris");
    }

    // AC5 — adopt doctor --clean removes twin-backed junk but leaves twin-less
    #[test]
    fn doctor_clean_removes_only_with_twin() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Junk with twin: create junk entry AND the real twin in home/.local/bin.
        make_junk_tree(home, "bin-with-twin");
        let twin_path = home.join(".local/bin/bin-with-twin");
        make_twin(&twin_path);

        // Junk without twin.
        make_junk_tree(home, "bin-no-twin");

        // run_doctor_with_home uses home for both junk scan and twin lookup.
        let result = run_doctor_with_home(home, true).unwrap();
        assert!(result, "expected debris to be reported");

        // bin-with-twin junk should be gone (had a twin).
        let junk_with = home.join("~/.local/bin/bin-with-twin");
        assert!(!junk_with.exists(), "junk with twin should have been removed");

        // bin-no-twin junk must remain (no twin).
        let junk_no = home.join("~/.local/bin/bin-no-twin");
        assert!(junk_no.exists(), "junk without twin must remain");
    }

    // AC6 — adopt doctor --clean does NOT recursively rm dirs with twin-less binaries
    #[test]
    fn doctor_clean_does_not_rm_nonempty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Only a twin-less junk binary — the parent dir must survive.
        make_junk_tree(home, "orphan-bin");

        run_doctor_with_home(home, true).unwrap();

        let junk_bin_dir = home.join("~/.local/bin");
        assert!(
            junk_bin_dir.exists(),
            "junk bin dir must survive when it still contains twin-less entries"
        );
    }
}
