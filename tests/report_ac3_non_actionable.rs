//! Report AC3: `installed-current` and `not-a-bin` produce no docket invocation.

use std::fs;
use std::process::Command;
use std::path::PathBuf;

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

fn mock_docket_dir(record: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("docket");
    let rec_str = record.to_string_lossy().replace('\'', "'\\''");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{rec_str}'\n");
    fs::write(&script_path, &script).expect("write mock");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");
    }
    dir
}

#[test]
fn installed_current_and_not_a_bin_produce_no_invocation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    let artifacts = vec![
        make_artifact("libonly", Verdict::NotABin, false, "/wm/libonly"),
        make_artifact("uptodate", Verdict::InstalledCurrent, false, "/wm/uptodate"),
    ];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", mock_dir.path().display());

    let output = Command::new(adopt_bin())
        .args([
            "report",
            "--run", "test-run-ac3",
            "--from-json", &json_path.to_string_lossy(),
        ])
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    assert!(
        output.status.success(),
        "adopt report failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let calls = if record.exists() {
        fs::read_to_string(&record).unwrap_or_default()
    } else {
        String::new()
    };
    assert!(calls.is_empty(), "docket was called for non-actionable: {calls}");
}
