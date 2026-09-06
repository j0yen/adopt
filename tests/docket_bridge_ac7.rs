//! AC7 (`docket_bridge_live)`: Given the real `docket` binary installed at
//! `~/.local/bin/docket`, when `adopt report --run <fresh-run-id>` runs on
//! this box, it exits 0 with no clap usage error in output.
//!
//! Runs against an isolated ledger (via `XDG_DATA_HOME`) so it never touches
//! the box's real docket state. Soft-skips (passes trivially) when the real
//! `docket` binary isn't installed, since this AC is inherently
//! environment-dependent (it is only meaningful "on this box").
//!
//! See PRD-adopt-exit-code-and-docket-bridge.md.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use adopt::types::ArtifactResult;

fn adopt_bin() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

/// Locate the real `docket` binary, preferring `~/.local/bin/docket` as the
/// PRD specifies, falling back to PATH.
fn real_docket() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/docket");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join("docket"))
            .find(|c| c.is_file())
    })
}

#[test]
fn adopt_report_against_real_docket_exits_0_no_usage_error() {
    let Some(docket_path) = real_docket() else {
        eprintln!("docket_bridge_ac7: no real `docket` binary found — skipping (env-dependent AC)");
        return;
    };
    let docket_dir = docket_path.parent().expect("docket has a parent dir").to_path_buf();

    let tmp = tempfile::tempdir().expect("tempdir");
    // Isolated ledger database — never touches the box's real docket state.
    let xdg_data_home = tmp.path().join("xdg-data");
    fs::create_dir_all(&xdg_data_home).expect("xdg data home");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", docket_dir.display());

    // Seed an open `adopt-scan-stale-binaries` finding directly via the real
    // docket binary so the auto-resolve call below has something to resolve.
    let seed = Command::new(&docket_path)
        .args([
            "report",
            "--run", "docket-bridge-ac7-seed",
            "--key", "adopt-scan-stale-binaries",
            "--title", "seed finding for docket_bridge_ac7",
            "--severity", "warn",
            "--evidence", "path:/tmp",
        ])
        .env("XDG_DATA_HOME", &xdg_data_home)
        .output()
        .expect("seed docket finding");
    assert!(
        seed.status.success(),
        "failed to seed docket finding: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    // Empty artifact list → lineage-behind count is 0 → adopt report attempts
    // to auto-resolve `adopt-scan-stale-binaries` via the real docket binary.
    let artifacts: Vec<ArtifactResult> = vec![];
    let json_path = tmp.path().join("artifacts.json");
    fs::write(&json_path, serde_json::to_string(&artifacts).expect("serialize")).expect("write");

    let out = Command::new(adopt_bin())
        .args(["report", "--run", "docket-bridge-ac7", "--from-json", &json_path.to_string_lossy()])
        .env("PATH", &new_path)
        .env("XDG_DATA_HOME", &xdg_data_home)
        .output()
        .expect("run adopt report");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "adopt report against real docket did not exit 0. stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("Usage: docket"),
        "stderr contains a clap usage error: {stderr}"
    );
}
