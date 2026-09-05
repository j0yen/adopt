//! AC2 (scan_exit_clean): Given a lineage where every artifact is
//! `installed-current` or `not-a-bin`, when `adopt scan` runs, the process
//! exits 0.
//!
//! See PRD-adopt-exit-code-and-docket-bridge.md.

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

/// A bin repo whose installed copy is newer than its newest src/ commit
/// (installed-current), plus a library-only repo (not-a-bin) alongside it.
fn make_clean_fixture(tmp: &TempDir, bin_name: &str, lib_name: &str) -> std::path::PathBuf {
    let repos_dir = tmp.path().join("repos");
    let fake_bin_dir = tmp.path().join("local").join("bin");
    let repo = repos_dir.join(bin_name);
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
    std::fs::create_dir_all(&fake_bin_dir).expect("bin dir");

    std::fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{bin_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"{bin_name}\"\npath = \"src/main.rs\"\n"
        ),
    )
    .expect("Cargo.toml");
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}").expect("main.rs");

    Command::new("git").args(["init"]).current_dir(&repo).output().expect("git init");
    Command::new("git")
        .args(["add", "src/main.rs"])
        .current_dir(&repo)
        .output()
        .expect("git add");
    Command::new("git")
        .args(["-c", "user.email=t@t.com", "-c", "user.name=T", "commit", "-m", "init src"])
        .current_dir(&repo)
        .output()
        .expect("git commit");

    // Ensure the installed binary's mtime is strictly after the commit.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let fake_bin = fake_bin_dir.join(bin_name);
    std::fs::write(&fake_bin, b"#!/bin/sh\n").expect("fake bin");

    // A library-only repo (not-a-bin) alongside the current bin.
    let lib_repo = repos_dir.join(lib_name);
    std::fs::create_dir_all(lib_repo.join("src")).expect("lib src dir");
    std::fs::write(
        lib_repo.join("Cargo.toml"),
        format!("[package]\nname = \"{lib_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"{lib_name}\"\n"),
    )
    .expect("lib Cargo.toml");
    std::fs::write(lib_repo.join("src/lib.rs"), b"").expect("lib.rs");
    Command::new("git").args(["init"]).current_dir(&lib_repo).output().expect("git init lib");

    fake_bin_dir
}

#[test]
fn scan_exit_clean_current_and_not_a_bin_exits_0() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-scanexit-ac2-bin";
    let lib_name = "adopt-test-scanexit-ac2-lib";
    let fake_bin_dir = make_clean_fixture(&tmp, bin_name, lib_name);
    let repos_dir = tmp.path().join("repos");

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{existing_path}", fake_bin_dir.display());

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--all"])
        .env("WM_WINTERMUTE_DIR", &repos_dir)
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    assert!(
        out.status.success(),
        "expected exit 0 when every artifact is installed-current/not-a-bin, got {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\n{stdout}"));

    let bin_entry = arr
        .iter()
        .find(|e| e["bin"].as_str() == Some(bin_name))
        .unwrap_or_else(|| panic!("{bin_name} not found:\n{stdout}"));
    assert_eq!(bin_entry["verdict"].as_str(), Some("installed-current"));

    let lib_entry = arr
        .iter()
        .find(|e| e["repo"].as_str().is_some_and(|r| r.contains(lib_name)));
    assert!(lib_entry.is_some(), "{lib_name} not found in --all output:\n{stdout}");
    assert_eq!(lib_entry.expect("checked above")["verdict"].as_str(), Some("not-a-bin"));
}
