//! AC6 (MUST): An unreadable manifest, malformed Cargo.toml, or missing repo
//! dir is skipped without aborting the scan (exit 0, other artifacts reported).

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

#[test]
fn malformed_cargo_toml_skipped_gracefully() {
    let tmp = TempDir::new().expect("tempdir");

    // A repo with a malformed Cargo.toml.
    let bad_repo = tmp.path().join("bad-repo");
    std::fs::create_dir_all(bad_repo.join("src")).expect("dir");
    std::fs::write(bad_repo.join("Cargo.toml"), b"[this is {{invalid toml}").expect("write");
    std::fs::write(bad_repo.join("src/main.rs"), b"fn main() {}").expect("write");

    // A valid repo alongside it (no [[bin]], so not-a-bin in --all).
    let good_repo = tmp.path().join("good-lib");
    std::fs::create_dir_all(good_repo.join("src")).expect("dir");
    std::fs::write(
        good_repo.join("Cargo.toml"),
        "[package]\nname = \"good-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"good_lib\"\n",
    )
    .expect("write");
    std::fs::write(good_repo.join("src/lib.rs"), b"").expect("write");

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--all"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .output()
        .expect("run adopt");

    // Must exit 0 even though bad-repo's Cargo.toml is malformed.
    assert!(
        out.status.success(),
        "adopt should exit 0 despite malformed Cargo.toml, got: {:?}",
        out.status
    );

    // Output must still be valid JSON.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));
}

#[test]
fn missing_repo_directory_skipped() {
    let tmp = TempDir::new().expect("tempdir");

    // Point to a dir that doesn't exist: scan should return empty array, exit 0.
    let nonexistent = tmp.path().join("does-not-exist");

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .env("WM_WINTERMUTE_DIR", &nonexistent)
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "exit 0 even when wintermute dir is missing: {:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));
    assert!(arr.is_empty(), "expected empty array when dir is missing");
}
