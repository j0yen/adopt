//! Report AC7: `--from-json` reads artifacts without re-running scan.
//!
//! Updated for scion-truth: an `InstalledStale + ClockFallback` artifact
//! goes into the `adopt-unmarked-installs` bucket (1 docket `report` call)
//! plus a `docket resolve` for `adopt-scan-stale-binaries` (lineage count=0),
//! for a total of 2 docket invocations.

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
fn from_json_reads_without_scan() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    // One clock-fallback stale artifact → adopt-unmarked-installs report
    // + adopt-scan-stale-binaries resolve (lineage count = 0) = 2 calls total.
    let artifacts = vec![
        make_artifact("mybin", Verdict::InstalledStale, false, "/wm/mybin"),
    ];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", mock_dir.path().display());

    let output = Command::new(adopt_bin())
        .args([
            "report",
            "--run", "test-run-ac7",
            "--from-json", &json_path.to_string_lossy(),
        ])
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    assert!(
        output.status.success(),
        "adopt report --from-json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let calls = fs::read_to_string(&record).unwrap_or_default();
    let count = calls.lines().count();
    // 1 report (adopt-unmarked-installs:mybin) + 1 resolve = 2
    assert_eq!(count, 2, "expected 2 docket calls (1 report + 1 resolve), got {count}: {calls}");

    // Verify one line starts with "report" and one with "resolve".
    let has_report = calls.lines().any(|l| l.starts_with("report"));
    let has_resolve = calls.lines().any(|l| l.starts_with("resolve"));
    assert!(has_report, "expected a 'report' docket call: {calls}");
    assert!(has_resolve, "expected a 'resolve' docket call: {calls}");
}
