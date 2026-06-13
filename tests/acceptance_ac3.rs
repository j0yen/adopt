//! AC3 (MUST): A binary whose installed mtime is NEWER than the repo's newest
//! src/ commit gets verdict `installed-current`.

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/debug/adopt");
    p
}

fn make_fixture(tmp: &TempDir, bin_name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = tmp.path().join("repos").join(bin_name);
    let fake_bin_dir = tmp.path().join("local").join("bin");
    std::fs::create_dir_all(repo.join("src")).expect("create src dir");
    std::fs::create_dir_all(&fake_bin_dir).expect("create bin dir");

    std::fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{bin_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"{bin_name}\"\npath = \"src/main.rs\"\n"
        ),
    )
    .expect("Cargo.toml");
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}").expect("main.rs");

    // Git init + commit.
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

    // Small sleep to ensure the fake bin's mtime is strictly after the commit timestamp.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Create a fake installed binary NEWER than the src/ commit.
    let fake_bin = fake_bin_dir.join(bin_name);
    std::fs::write(&fake_bin, b"#!/bin/sh\necho hello").expect("fake bin");

    // Force its mtime to the future (1 day ahead) by sleeping briefly then touching.
    // Actually: set it explicitly via filetime or rely on it being written after commit.
    // The commit is in the past; the file we just wrote is NOW (after commit). So it's newer.

    (repo, fake_bin)
}

#[test]
fn binary_newer_than_src_commit_is_installed_current() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-ac3-xyz";
    let (_repo, _fake_bin) = make_fixture(&tmp, bin_name);

    // The fake local/bin is in tmp.path()/local/bin.
    // The scan walks WM_WINTERMUTE_DIR for repos.
    // It checks ~/.local/bin, ~/.cargo/bin, and PATH.
    // We override PATH to include our fake bin dir.
    let fake_bin_dir = tmp.path().join("local").join("bin");
    let repos_dir = tmp.path().join("repos");

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{existing_path}", fake_bin_dir.display());

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--all"])
        .env("WM_WINTERMUTE_DIR", &repos_dir)
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "{:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));

    let entry = arr.iter().find(|e| e["bin"].as_str() == Some(bin_name))
        .unwrap_or_else(|| panic!("{bin_name} not found in:\n{stdout}"));

    assert_eq!(
        entry["verdict"].as_str(),
        Some("installed-current"),
        "expected installed-current, got: {} — the installed binary should be newer than the src/ commit",
        entry["verdict"]
    );
}
