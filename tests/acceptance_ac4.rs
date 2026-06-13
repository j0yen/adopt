//! AC4 (MUST): A binary whose installed mtime is OLDER than the repo's newest
//! src/ commit gets verdict `installed-stale`.

use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/debug/adopt");
    p
}

#[test]
fn binary_older_than_src_commit_is_installed_stale() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-ac4-xyz";

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

    // Create the fake installed binary FIRST (old).
    let fake_bin = fake_bin_dir.join(bin_name);
    std::fs::write(&fake_bin, b"#!/bin/sh").expect("fake bin");

    // Set fake bin mtime to 10 days ago.
    let old_time = SystemTime::now() - Duration::from_secs(10 * 86400);
    let old_ft = filetime::FileTime::from_system_time(old_time);
    filetime::set_file_mtime(&fake_bin, old_ft).expect("set mtime");

    // Git init + commit src/main.rs (AFTER the fake binary was written → newer src ts).
    Command::new("git").args(["init"]).current_dir(&repo).output().expect("git init");
    // Small sleep to ensure commit ts > old_ft (git uses whole seconds).
    std::thread::sleep(Duration::from_millis(1100));
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
        .unwrap_or_else(|| panic!("{bin_name} not found:\n{stdout}"));

    assert_eq!(
        entry["verdict"].as_str(),
        Some("installed-stale"),
        "expected installed-stale, got: {}",
        entry["verdict"]
    );
}
