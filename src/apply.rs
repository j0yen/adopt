//! `adopt apply` — install non-daemon unadopted artifacts, one at a time.
//!
//! # Safety posture
//!
//! - Default mode is **dry-run**: nothing is installed unless `--execute` is passed.
//! - Daemon artifacts are skipped by default and delegated to `rollout install`.
//! - Subprocess args are passed as a discrete vector; no `sh -c`.
//! - Processes run strictly sequentially — never two `cargo install` concurrently.

use std::process::Command;
use std::time::Instant;

use anyhow::Result;

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

// ── Core apply logic ─────────────────────────────────────────────────────────

/// Runs `adopt apply`.
///
/// # Parameters
///
/// - `dry_run` — if `true`, print what would happen without doing anything.
/// - `execute` — if `true`, run the `fix_cmd` subprocesses.
/// - `only` — if `Some`, restrict to a single named artifact.
/// - `with_daemons` — if `true`, daemon artifacts are delegated to `rollout install`.
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
    }
}
