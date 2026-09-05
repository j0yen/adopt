//! AC6 (docket_bridge_error_log): Given a stub `docket` that exits 2 on any
//! argv, when `adopt report` runs, stderr contains the full argv adopt
//! attempted before the exit-status line — so the next CLI drift is
//! diagnosable from the error message alone.
//!
//! See PRD-adopt-exit-code-and-docket-bridge.md.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use adopt::types::{ArtifactResult, FreshnessBasis, Verdict};

fn adopt_bin() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

/// A stub `docket` that always exits 2, regardless of argv — simulates a
/// clap usage error (or any other docket CLI rejection).
fn always_fails_docket_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("docket");
    fs::write(&script_path, "#!/bin/sh\nexit 2\n").expect("write stub docket");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod stub docket");
    }
    dir
}

fn make_artifact(bin: &str, verdict: Verdict, repo: &str) -> ArtifactResult {
    ArtifactResult {
        repo: repo.to_owned(),
        bin: bin.to_owned(),
        verdict,
        installed_path: None,
        is_daemon: false,
        source_commit_ts: None,
        installed_ts: None,
        fix_cmd: String::new(),
        age_vs_head: None,
        freshness_basis: FreshnessBasis::ClockFallback,
    }
}

#[test]
fn failed_docket_call_logs_argv_before_exit_status_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mock_dir = always_fails_docket_dir();

    let artifacts = vec![make_artifact("rollout", Verdict::NotInstalled, "/wm/rollout")];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", mock_dir.path().display());

    let out = Command::new(adopt_bin())
        .args(["report", "--run", "docket-bridge-ac6", "--from-json", &json_path.to_string_lossy()])
        .env("PATH", &new_path)
        .output()
        .expect("run adopt report");

    assert!(
        !out.status.success(),
        "expected adopt report to fail when docket rejects every call"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);

    let argv_pos = stderr
        .find("adopt: docket argv:")
        .unwrap_or_else(|| panic!("stderr missing the argv line, got: {stderr}"));
    // The argv line must show the actual command attempted.
    assert!(
        stderr.contains("docket report") || stderr.contains("docket resolve"),
        "argv line must show the attempted docket command, got: {stderr}"
    );

    let exit_status_pos = stderr
        .find("docket exited with status")
        .unwrap_or_else(|| panic!("stderr missing the exit-status line, got: {stderr}"));

    assert!(
        argv_pos < exit_status_pos,
        "argv line must appear BEFORE the exit-status line, got stderr:\n{stderr}"
    );
}
