//! AC10 (MUST): `adopt --version` and `adopt --help` exit 0.

use std::process::Command;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

#[test]
fn version_exits_zero() {
    let out = Command::new(adopt_bin())
        .arg("--version")
        .output()
        .expect("run adopt --version");
    assert!(out.status.success(), "adopt --version should exit 0, got: {:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("adopt"), "version output should contain 'adopt': {stdout}");
}

#[test]
fn help_exits_zero() {
    let out = Command::new(adopt_bin())
        .arg("--help")
        .output()
        .expect("run adopt --help");
    assert!(out.status.success(), "adopt --help should exit 0, got: {:?}", out.status);
}
