//! Report AC1: `--dry-run` prints one command per actionable artifact; executes nothing.
//!
//! Updated for scion-truth:
//! - `NotInstalled` → 1 `docket report` line
//! - `InstalledStale(ClockFallback)` → 1 `docket report` line (adopt-unmarked-installs)
//! - lineage count == 0 → 1 `docket resolve` line
//! - `InstalledCurrent` → skipped (no line)
//! Total: 3 printed lines.

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
fn report_dry_run_prints_commands_executes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    let artifacts = vec![
        make_artifact("rollout", Verdict::NotInstalled, false, "/wm/rollout"),
        make_artifact("warden", Verdict::InstalledStale, false, "/wm/warden"), // ClockFallback
        make_artifact("adopt", Verdict::InstalledCurrent, false, "/wm/adopt"),
    ];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", mock_dir.path().display());

    let output = Command::new(adopt_bin())
        .args([
            "report",
            "--run", "test-run-ac1",
            "--dry-run",
            "--from-json", &json_path.to_string_lossy(),
        ])
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    assert!(
        output.status.success(),
        "adopt report --dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    // scion-truth: not-installed(1) + unmarked(1) + resolve(1) = 3 lines; installed-current skipped.
    assert_eq!(lines.len(), 3, "expected 3 dry-run lines (2 reports + 1 resolve), got:\n{stdout}");

    // docket was NOT called (dry-run only prints).
    let executed = if record.exists() {
        fs::read_to_string(&record).unwrap_or_default()
    } else {
        String::new()
    };
    assert!(
        executed.is_empty(),
        "docket was invoked during --dry-run: {executed}"
    );
}
