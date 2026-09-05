//! AC5 (docket_bridge_resolve_argv): Given a stub `docket` binary on PATH
//! that records its argv, when `adopt report` resolves a finding, the stub
//! receives exactly `resolve <slug>` (plus `--reason <text>` iff a reason is
//! passed) and no `--run` or `--key` token — matching docket's real
//! `resolve [OPTIONS] <KEY>` signature.
//!
//! See PRD-adopt-exit-code-and-docket-bridge.md.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use adopt::report::build_resolve_args;
use adopt::types::ArtifactResult;

fn adopt_bin() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

fn mock_docket_dir(record: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("docket");
    let rec_str = record.to_string_lossy().replace('\'', "'\\''");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{rec_str}'\n");
    fs::write(&script_path, &script).expect("write mock docket");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod mock docket");
    }
    dir
}

// ── Unit-level: build_resolve_args must produce docket's real positional
//    signature, exactly as spelled out by AC5 with slug `wm-node`. ──────────

#[test]
fn build_resolve_args_no_reason_is_exactly_resolve_and_slug() {
    let args = build_resolve_args("wm-node", None);
    assert_eq!(args, vec!["resolve".to_owned(), "wm-node".to_owned()]);
    assert!(!args.iter().any(|a| a == "--run"), "must not contain --run");
    assert!(!args.iter().any(|a| a == "--key"), "must not contain --key");
}

#[test]
fn build_resolve_args_with_reason_appends_reason_flag() {
    let args = build_resolve_args("wm-node", Some("no longer behind"));
    assert_eq!(
        args,
        vec![
            "resolve".to_owned(),
            "wm-node".to_owned(),
            "--reason".to_owned(),
            "no longer behind".to_owned(),
        ]
    );
    assert!(!args.iter().any(|a| a == "--run"), "must not contain --run");
    assert!(!args.iter().any(|a| a == "--key"), "must not contain --key");
}

// ── Integration-level: adopt report's real auto-resolve call reaches the
//    stub docket with the same no-`--run`/`--key` shape. ────────────────────

#[test]
fn adopt_report_resolve_call_has_no_run_or_key_token() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    // Empty artifact list → lineage-behind count is 0 → auto-resolve fires.
    let artifacts: Vec<ArtifactResult> = vec![];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", mock_dir.path().display());

    let out = Command::new(adopt_bin())
        .args(["report", "--run", "docket-bridge-ac5", "--from-json", &json_path.to_string_lossy()])
        .env("PATH", &new_path)
        .output()
        .expect("run adopt report");

    assert!(
        out.status.success(),
        "adopt report failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let calls = fs::read_to_string(&record).unwrap_or_default();
    let resolve_line = calls
        .lines()
        .find(|l| l.starts_with("resolve"))
        .unwrap_or_else(|| panic!("no resolve line recorded, got: {calls}"));

    assert_eq!(
        resolve_line, "resolve adopt-scan-stale-binaries",
        "stub must receive exactly 'resolve <slug>' with no --run/--key token, got: {resolve_line}"
    );
}
