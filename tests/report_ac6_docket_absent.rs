//! Report AC6: docket absent from PATH → non-zero exit, message names `docket`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use adopt::types::{ArtifactResult, FreshnessBasis, Verdict};

fn adopt_bin() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

fn make_artifact(bin: &str, verdict: Verdict, is_daemon: bool, repo: &str) -> ArtifactResult {
    ArtifactResult {
        repo: repo.to_owned(),
        bin: bin.to_owned(),
        verdict,
        installed_path: None,
        is_daemon,
        source_commit_ts: None,
        installed_ts: None,
        fix_cmd: String::new(),
        age_vs_head: None,
        freshness_basis: FreshnessBasis::ClockFallback,
    }
}

#[test]
fn docket_absent_exits_nonzero_names_docket() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let artifacts = vec![
        make_artifact("rollout", Verdict::NotInstalled, false, "/wm/rollout"),
    ];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    // Use only the tmp dir as PATH — no docket there.
    let output = Command::new(adopt_bin())
        .args([
            "report",
            "--run", "test-run-ac6",
            "--from-json", &json_path.to_string_lossy(),
        ])
        .env("PATH", tmp.path())
        .output()
        .expect("run adopt");

    assert!(
        !output.status.success(),
        "adopt report should fail when docket is absent"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("docket"),
        "error message must name 'docket', got: {stderr}"
    );
}
