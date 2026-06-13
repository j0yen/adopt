//! AC5 (MUST): A library-only repo (no [[bin]], no default-run) is reported
//! `not-a-bin` and excluded from default output (shown only with --all).

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

fn make_lib_repo(tmp: &TempDir, name: &str) -> std::path::PathBuf {
    let repo = tmp.path().join(name);
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
    std::fs::write(
        repo.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"{name}\"\n"),
    )
    .expect("Cargo.toml");
    std::fs::write(repo.join("src/lib.rs"), b"").expect("lib.rs");
    Command::new("git").args(["init"]).current_dir(&repo).output().expect("git init");
    repo
}

#[test]
fn library_only_excluded_from_default_output() {
    let tmp = TempDir::new().expect("tempdir");
    let name = "adopt-test-ac5-lib";
    make_lib_repo(&tmp, name);

    // Default output should NOT include not-a-bin.
    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "{:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));

    let found = arr.iter().any(|e| e["bin"].as_str() == Some(name) || e["repo"].as_str().map_or(false, |r| r.contains(name)));
    assert!(!found, "library-only repo {name} should not appear in default scan output");
}

#[test]
fn library_only_shown_with_all_flag() {
    let tmp = TempDir::new().expect("tempdir");
    let name = "adopt-test-ac5-lib2";
    make_lib_repo(&tmp, name);

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--all"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "{:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));

    // Find the entry for the lib repo.
    let entry = arr.iter().find(|e| {
        e["repo"].as_str().map_or(false, |r| r.contains(name))
    });
    let entry = entry.unwrap_or_else(|| panic!("{name} not found in --all output:\n{stdout}"));
    assert_eq!(entry["verdict"].as_str(), Some("not-a-bin"), "expected not-a-bin, got: {}", entry["verdict"]);
}
