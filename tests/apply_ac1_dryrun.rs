//! AC1: `adopt apply` (no --execute) = dry-run, no binaries installed.

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/debug/adopt");
    p
}

#[test]
fn apply_default_is_dryrun_exits_zero() {
    // Point scan at an empty wintermute dir so no real artifacts are found.
    // This makes the test deterministic and fast.
    let tmp = TempDir::new().expect("tempdir");

    let out = Command::new(adopt_bin())
        .args(["apply"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .output()
        .expect("failed to run adopt apply");

    // dry-run should never exit non-zero unless the scan itself errors
    assert!(
        out.status.success(),
        "adopt apply (dry-run) exited non-zero: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    // Dry-run output must not contain "FAILED" lines
    assert!(
        !stdout.contains("FAILED"),
        "dry-run should not produce any FAILED lines: {stdout}"
    );
}
