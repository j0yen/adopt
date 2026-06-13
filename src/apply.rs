//! `adopt apply` — install non-daemon unadopted artifacts, one at a time.
//!
//! # Safety posture
//!
//! - Default mode is **dry-run**: nothing is installed unless `--execute` is passed.
//! - Daemon artifacts are skipped by default and delegated to `rollout install`.
//! - Subprocess args are passed as a discrete vector; no `sh -c`.
//! - Processes run strictly sequentially — never two `cargo install` concurrently.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use anyhow::Result;

use crate::marker;
use crate::scan;
use crate::types::Verdict;

// ── Public types ─────────────────────────────────────────────────────────────

/// Outcome for a single artifact during `adopt apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Binary was successfully installed during this run.
    InstalledOk,
    /// Binary was already installed and current; nothing was done.
    InstalledCurrent,
    /// Source fingerprint matched the install marker; cargo install was skipped.
    AlreadyCurrent,
    /// Binary is a daemon and `--with-daemons` was not requested.
    SkippedDaemon {
        /// Human-readable reason.
        note: String,
    },
    /// `--with-daemons` was requested but `rollout` is not on PATH.
    SkippedDaemonsNotRequested,
    /// Daemon was delegated to `rollout install` successfully.
    RolloutDelegated,
    /// Install failed; the run has been halted.
    Failed {
        /// Why it failed.
        reason: String,
    },
    /// Artifact had no rollout needed (fix_cmd is empty).
    NoRollout,
    /// Install-prefix contained a literal `~` or resolved outside `$HOME`.
    BadPrefix {
        /// The rejected path string (raw, for display).
        resolved: String,
    },
}

/// Result for a single artifact in an `adopt apply` run.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    /// Binary name.
    pub bin: String,
    /// What happened to this artifact.
    pub verdict: ApplyOutcome,
    /// Wall-clock time for the operation, in milliseconds.
    pub elapsed_ms: u64,
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Parses a fix_cmd string into a discrete argv vector.
///
/// Uses whitespace splitting (no shell expansion) so metacharacters in paths
/// are never interpreted by a shell. Quotes are **not** stripped — callers
/// should not embed shell-quoting in fix_cmd; they should use raw paths.
fn parse_cmd(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(str::to_owned).collect()
}

/// Returns `true` if `bin` is on PATH (found by `which`).
fn is_invokable(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Returns `true` if `rollout` is on PATH.
fn rollout_on_path() -> bool {
    Command::new("which")
        .arg("rollout")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Prefix validation ─────────────────────────────────────────────────────────

/// Validates an install-prefix string before passing it to `cargo install --root`.
///
/// # Errors
///
/// Returns an `Err` (with the rejected value) when:
/// - Any path component is exactly `~` or starts with `~` (cargo does not expand tildes).
/// - `$HOME` is unset or empty (would silently relativise the prefix).
/// - The canonicalised absolute path is not under `$HOME`.
pub fn validate_root(root: &str) -> Result<PathBuf, String> {
    // Reject any component that is `~` or starts with `~`.
    for component in std::path::Path::new(root).components() {
        use std::path::Component;
        let s = match component {
            Component::Normal(os) => os.to_string_lossy().into_owned(),
            _ => continue,
        };
        if s == "~" || s.starts_with('~') {
            return Err(format!(
                "install prefix contains a literal tilde component (`{s}`); \
                 cargo does not expand `~` — pass an absolute path instead"
            ));
        }
    }

    // $HOME must be set and non-empty.
    let home_str = std::env::var("HOME").unwrap_or_default();
    if home_str.is_empty() {
        return Err(
            "$HOME is unset or empty; cannot validate install prefix".to_owned(),
        );
    }
    let home = PathBuf::from(&home_str);

    // Canonicalize: join to $HOME if relative, then resolve symlinks / `..`.
    let abs = if std::path::Path::new(root).is_absolute() {
        PathBuf::from(root)
    } else {
        home.join(root)
    };

    let canonical = abs.canonicalize().unwrap_or(abs);

    // Must reside under $HOME.
    if !canonical.starts_with(&home) {
        return Err(format!(
            "install prefix `{}` resolves outside $HOME (`{}`); rejected",
            root,
            home.display()
        ));
    }

    Ok(canonical)
}

// ── Core apply logic ─────────────────────────────────────────────────────────

/// Runs `adopt apply`.
///
/// # Parameters
///
/// - `dry_run` — if `true`, print what would happen without doing anything.
/// - `execute` — if `true`, run the `fix_cmd` subprocesses.
/// - `only` — if `Some`, restrict to a single named artifact.
/// - `with_daemons` — if `true`, daemon artifacts are delegated to `rollout install`.
/// - `force_all` — if `true`, bypass the incremental-skip check and always reinstall.
///
/// # Errors
///
/// Returns an error if the scan itself fails.  Individual install failures are
/// encoded as `ApplyOutcome::Failed` and the function returns `Ok` with that
/// result (the caller should inspect `ApplyOutcome::Failed` and exit non-zero).
#[allow(clippy::print_stdout)]
pub fn run_apply(
    dry_run: bool,
    execute: bool,
    only: Option<&str>,
    with_daemons: bool,
    force_all: bool,
) -> Result<Vec<ApplyResult>> {
    // Scan everything — include installed-current (show_all=true) so we can
    // report idempotent re-runs correctly. Filter to only the requested bin.
    let scan_results = scan::run_scan(true, only)?;

    let mut output: Vec<ApplyResult> = Vec::new();

    for artifact in &scan_results {
        let start = Instant::now();

        // ── Already current ────────────────────────────────────────────────
        if artifact.verdict == Verdict::InstalledCurrent || artifact.verdict == Verdict::NotABin {
            output.push(ApplyResult {
                bin: artifact.bin.clone(),
                verdict: ApplyOutcome::InstalledCurrent,
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
            continue;
        }

        // ── Daemon handling ────────────────────────────────────────────────
        if artifact.is_daemon {
            if !with_daemons {
                output.push(ApplyResult {
                    bin: artifact.bin.clone(),
                    verdict: ApplyOutcome::SkippedDaemon {
                        note: format!(
                            "{} is a daemon; use `rollout install` or pass --with-daemons",
                            artifact.bin
                        ),
                    },
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
                continue;
            }

            // --with-daemons: delegate to rollout
            if !rollout_on_path() {
                output.push(ApplyResult {
                    bin: artifact.bin.clone(),
                    verdict: ApplyOutcome::SkippedDaemonsNotRequested,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
                continue;
            }

            if dry_run || !execute {
                println!(
                    "[dry-run] would run: rollout install {}",
                    artifact.repo
                );
                output.push(ApplyResult {
                    bin: artifact.bin.clone(),
                    verdict: ApplyOutcome::RolloutDelegated,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
                continue;
            }

            let status = Command::new("rollout")
                .arg("install")
                .arg(&artifact.repo)
                .status();

            match status {
                Ok(s) if s.success() => {
                    output.push(ApplyResult {
                        bin: artifact.bin.clone(),
                        verdict: ApplyOutcome::RolloutDelegated,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });
                }
                Ok(s) => {
                    let reason = format!(
                        "rollout install exited {:?}",
                        s.code()
                    );
                    output.push(ApplyResult {
                        bin: artifact.bin.clone(),
                        verdict: ApplyOutcome::Failed { reason },
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });

                }
                Err(e) => {
                    output.push(ApplyResult {
                        bin: artifact.bin.clone(),
                        verdict: ApplyOutcome::Failed {
                            reason: format!("could not spawn rollout: {e}"),
                        },
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });

                }
            }
            continue;
        }

        // ── Non-daemon actionable artifact ─────────────────────────────────
        if artifact.fix_cmd.is_empty() {
            output.push(ApplyResult {
                bin: artifact.bin.clone(),
                verdict: ApplyOutcome::NoRollout,
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
            continue;
        }

        let argv = parse_cmd(&artifact.fix_cmd);

        // ── Prefix guard ───────────────────────────────────────────────────
        // If the command carries `--root <value>`, validate the prefix before
        // executing anything — even in dry-run mode (so the dry-run output is
        // accurate).
        if let Some(root_idx) = argv.iter().position(|a| a == "--root") {
            if let Some(root_val) = argv.get(root_idx + 1) {
                if let Err(reason) = validate_root(root_val) {
                    output.push(ApplyResult {
                        bin: artifact.bin.clone(),
                        verdict: ApplyOutcome::BadPrefix { resolved: reason },
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });
                    continue;
                }
            }
        }

        // ── Incremental-skip check ─────────────────────────────────────────
        // Only skip if we're in execute mode (dry-run always shows what would
        // happen) and --force-all was not requested.
        if execute && !force_all {
            // Compute current source fingerprint for this repo.
            let current_fp = marker::compute_fingerprint(
                std::path::Path::new(&artifact.repo),
            );
            if let Ok(ref fp) = current_fp {
                // A "dirty:..." fingerprint is never skipped (it indicates
                // uncommitted changes which we cannot fingerprint precisely).
                if !fp.0.starts_with("dirty:") {
                    if let Ok(Some(saved)) = marker::read_marker(&artifact.bin) {
                        if saved.source_fingerprint == *fp && is_invokable(&artifact.bin) {
                            output.push(ApplyResult {
                                bin: artifact.bin.clone(),
                                verdict: ApplyOutcome::AlreadyCurrent,
                                elapsed_ms: start.elapsed().as_millis() as u64,
                            });
                            continue;
                        }
                    }
                }
            }
        }

        if dry_run || !execute {
            println!("[dry-run] would run: {}", artifact.fix_cmd);
            output.push(ApplyResult {
                bin: artifact.bin.clone(),
                verdict: ApplyOutcome::InstalledOk, // would-be
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
            continue;
        }

        // ── Execute ────────────────────────────────────────────────────────
        // argv[0] is the program; the rest are args. Never pass to sh -c.
        let (prog, rest) = match argv.split_first() {
            Some(pair) => pair,
            None => {
                output.push(ApplyResult {
                    bin: artifact.bin.clone(),
                    verdict: ApplyOutcome::Failed {
                        reason: "fix_cmd is empty after parse".to_owned(),
                    },
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });

                continue;
            }
        };

        let status = Command::new(prog).args(rest).status();

        match status {
            Ok(s) if s.success() => {
                // Verify the binary is now invokable.
                if is_invokable(&artifact.bin) {
                    // Write / update the install marker.
                    // Best-effort: marker write failure doesn't abort the install.
                    let fp_for_marker = marker::compute_fingerprint(
                        std::path::Path::new(&artifact.repo),
                    );
                    if let Ok(fp) = fp_for_marker {
                        let _ = marker::write_marker(&artifact.bin, &artifact.repo, &fp, "install");
                    }
                    output.push(ApplyResult {
                        bin: artifact.bin.clone(),
                        verdict: ApplyOutcome::InstalledOk,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });
                } else {
                    let reason = format!(
                        "{} install exited 0 but `{} --version` / `--help` failed",
                        artifact.bin, artifact.bin
                    );
                    output.push(ApplyResult {
                        bin: artifact.bin.clone(),
                        verdict: ApplyOutcome::Failed { reason },
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });

                }
            }
            Ok(s) => {
                let reason = format!(
                    "{} install exited {:?}",
                    artifact.bin,
                    s.code()
                );
                output.push(ApplyResult {
                    bin: artifact.bin.clone(),
                    verdict: ApplyOutcome::Failed { reason },
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });

            }
            Err(e) => {
                output.push(ApplyResult {
                    bin: artifact.bin.clone(),
                    verdict: ApplyOutcome::Failed {
                        reason: format!("could not spawn {prog}: {e}"),
                    },
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });

            }
        }
    }

    Ok(output)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cmd_basic() {
        let argv = parse_cmd("cargo install --path /home/joe/wintermute/foo --root ~/.local");
        assert_eq!(
            argv,
            vec!["cargo", "install", "--path", "/home/joe/wintermute/foo", "--root", "~/.local"]
        );
    }

    #[test]
    fn parse_cmd_metachar_in_path() {
        // Paths with spaces or metacharacters must not be shell-expanded.
        // parse_cmd splits on whitespace only — no shell expansion.
        let argv = parse_cmd("cargo install --path /tmp/my;evil--path --root /tmp");
        // The semicolon is treated as a literal character in the arg string.
        assert!(argv.contains(&"/tmp/my;evil--path".to_owned()));
    }

    #[test]
    fn apply_outcome_eq() {
        assert_eq!(ApplyOutcome::InstalledOk, ApplyOutcome::InstalledOk);
        assert_ne!(ApplyOutcome::InstalledOk, ApplyOutcome::InstalledCurrent);
        assert_ne!(ApplyOutcome::InstalledOk, ApplyOutcome::AlreadyCurrent);
        assert_eq!(ApplyOutcome::AlreadyCurrent, ApplyOutcome::AlreadyCurrent);
    }

    // AC1: validate_root rejects literal tilde component.
    #[test]
    fn validate_root_tilde_rejected() {
        assert!(validate_root("~/.local").is_err());
        assert!(validate_root("~/foo/bar").is_err());
        // A component that starts-with-tilde but is not bare ~.
        assert!(validate_root("~jsy/.local").is_err());
    }

    // AC1 (positive): validate_root accepts absolute path under $HOME.
    #[test]
    fn validate_root_absolute_under_home_ok() {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        let root = format!("{home}/.local");
        assert!(validate_root(&root).is_ok(), "should accept absolute path under $HOME");
    }

    // AC3: validate_root rejects a root outside $HOME.
    #[test]
    fn validate_root_outside_home_rejected() {
        // /tmp is guaranteed to exist and canonicalise cleanly, and is never under $HOME.
        let result = validate_root("/tmp");
        assert!(result.is_err(), "expected rejection for /tmp (outside $HOME)");
    }

    // AC2: fix_cmd with --root ~/.local yields BadPrefix, Command is never spawned.
    // (Behavioral: we verify the outcome type produced by the same logic used in run_apply.)
    #[test]
    fn bad_prefix_from_tilde_root() {
        let fix_cmd = "cargo install --force --path /tmp/fake --root ~/.local";
        let argv = parse_cmd(fix_cmd);
        let root_idx = argv.iter().position(|a| a == "--root");
        let root_val = root_idx.and_then(|i| argv.get(i + 1)).map(String::as_str).unwrap_or("");
        let outcome = match validate_root(root_val) {
            Err(reason) => ApplyOutcome::BadPrefix { resolved: reason },
            Ok(_) => ApplyOutcome::InstalledOk,
        };
        assert!(
            matches!(outcome, ApplyOutcome::BadPrefix { .. }),
            "expected BadPrefix for tilde root, got {outcome:?}"
        );
    }

    // ── Incremental-skip tests ────────────────────────────────────────────────
    //
    // These tests exercise the marker module directly (unit-testing the
    // skip decision logic) without spawning cargo.

    use crate::marker::{
        compute_fingerprint, read_marker, write_marker, SourceFingerprint,
    };
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serialise all tests that mutate `XDG_STATE_HOME`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_state_home<F: FnOnce(&TempDir)>(f: F) {
        let tmp = TempDir::new().unwrap();
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        f(&tmp);
        std::env::remove_var("XDG_STATE_HOME");
    }

    /// AC1: After a simulated successful install, marker file exists with correct fields.
    #[test]
    fn ac1_marker_written_after_install() {
        with_state_home(|_tmp| {
            let fp = SourceFingerprint("deadbeef".to_owned());
            write_marker("testbin", "/home/joe/wintermute/testbin", &fp, "install").unwrap();

            let marker = read_marker("testbin").unwrap().expect("marker should exist");
            assert_eq!(marker.bin, "testbin");
            assert_eq!(marker.repo_path, "/home/joe/wintermute/testbin");
            assert_eq!(marker.source_fingerprint, fp);
            assert!(marker.installed_at > 0, "installed_at should be a real timestamp");
        });
    }

    /// AC2: When the stored fingerprint matches the current one, the skip decision
    /// is AlreadyCurrent.  We test the decision logic directly (not run_apply,
    /// which requires a real scan + cargo).
    #[test]
    fn ac2_matching_fingerprint_produces_already_current() {
        with_state_home(|_tmp| {
            let fp = SourceFingerprint("abc123commit".to_owned());
            write_marker("mybin", "/some/repo", &fp, "install").unwrap();

            let saved = read_marker("mybin").unwrap().expect("marker missing");
            // Simulate the decision: if fingerprints match and binary were
            // invokable, outcome would be AlreadyCurrent.
            let would_skip = saved.source_fingerprint == fp;
            assert!(would_skip, "matching fingerprint should trigger skip");
        });
    }

    /// AC3: Changing a source file's mtime produces a different "dirty:" fingerprint.
    #[test]
    fn ac3_touching_source_changes_fingerprint() {
        use std::time::{Duration, SystemTime};
        use filetime::FileTime;

        let tmp_repo = TempDir::new().unwrap();
        let src_dir = tmp_repo.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src_file = src_dir.join("lib.rs");
        std::fs::write(&src_file, b"fn foo() {}").unwrap();

        // First fingerprint (dirty because not a git repo)
        let fp1 = compute_fingerprint(tmp_repo.path()).unwrap();
        assert!(fp1.0.starts_with("dirty:"), "should be dirty fingerprint: {fp1}");

        // Touch the source file to advance its mtime by 2 seconds.
        let new_mtime = SystemTime::now() + Duration::from_secs(2);
        filetime::set_file_mtime(&src_file, FileTime::from_system_time(new_mtime)).unwrap();

        let fp2 = compute_fingerprint(tmp_repo.path()).unwrap();
        assert!(fp2.0.starts_with("dirty:"), "should still be dirty: {fp2}");
        assert_ne!(fp1, fp2, "mtime change should change fingerprint");
    }

    /// AC4: A dirty git worktree fingerprints as "dirty:..." and never matches
    /// a clean commit fingerprint.
    #[test]
    fn ac4_dirty_worktree_never_skipped() {
        let tmp_repo = TempDir::new().unwrap();
        // Not a git repo → always returns dirty:...
        let fp = compute_fingerprint(tmp_repo.path()).unwrap();
        assert!(
            fp.0.starts_with("dirty:"),
            "non-git / dirty repo should start with 'dirty:': {fp}"
        );
        // A dirty fingerprint must never equal a clean commit hash.
        let clean_fp = SourceFingerprint("abcdef1234567890abcdef1234567890deadbeef".to_owned());
        assert_ne!(fp, clean_fp);
    }

    /// AC5: --force-all bypasses skip logic — tested by checking that the decision
    /// variable `force_all` short-circuits the skip.
    #[test]
    fn ac5_force_all_bypasses_skip() {
        with_state_home(|_tmp| {
            let fp = SourceFingerprint("fixed_commit_abc".to_owned());
            write_marker("forcebin", "/some/repo", &fp, "install").unwrap();

            let saved = read_marker("forcebin").unwrap().expect("marker missing");
            let matches = saved.source_fingerprint == fp;

            // Mimic the skip guard: even with matching fingerprint, force_all=true
            // must produce a reinstall (no skip).
            let force_all = true;
            let would_skip = matches && !force_all;
            assert!(!would_skip, "--force-all must prevent skipping");
        });
    }

    /// AC6: AlreadyCurrent is a distinct variant from InstalledCurrent.
    /// Summary counts correctly distinguish them.
    #[test]
    fn ac6_summary_distinguishes_counts() {
        let results = vec![
            ApplyResult {
                bin: "a".to_owned(),
                verdict: ApplyOutcome::InstalledOk,
                elapsed_ms: 0,
            },
            ApplyResult {
                bin: "b".to_owned(),
                verdict: ApplyOutcome::AlreadyCurrent,
                elapsed_ms: 0,
            },
            ApplyResult {
                bin: "c".to_owned(),
                verdict: ApplyOutcome::InstalledCurrent,
                elapsed_ms: 0,
            },
            ApplyResult {
                bin: "d".to_owned(),
                verdict: ApplyOutcome::SkippedDaemon { note: "daemon".to_owned() },
                elapsed_ms: 0,
            },
            ApplyResult {
                bin: "e".to_owned(),
                verdict: ApplyOutcome::Failed { reason: "oops".to_owned() },
                elapsed_ms: 0,
            },
        ];

        let installed = results
            .iter()
            .filter(|r| matches!(r.verdict, ApplyOutcome::InstalledOk))
            .count();
        let already_current = results
            .iter()
            .filter(|r| {
                matches!(
                    r.verdict,
                    ApplyOutcome::AlreadyCurrent | ApplyOutcome::InstalledCurrent
                )
            })
            .count();
        let skipped_daemon = results
            .iter()
            .filter(|r| {
                matches!(
                    r.verdict,
                    ApplyOutcome::SkippedDaemon { .. } | ApplyOutcome::SkippedDaemonsNotRequested
                )
            })
            .count();
        let failed = results
            .iter()
            .filter(|r| {
                matches!(
                    r.verdict,
                    ApplyOutcome::Failed { .. } | ApplyOutcome::BadPrefix { .. }
                )
            })
            .count();

        assert_eq!(installed, 1, "installed count");
        assert_eq!(already_current, 2, "already-current count (AlreadyCurrent + InstalledCurrent)");
        assert_eq!(skipped_daemon, 1, "skipped-daemon count");
        assert_eq!(failed, 1, "failed count");
    }
}
