//! AC3 (`scan_exit_stale)`: Given a lineage with at least one `installed-stale`
//! artifact and zero `not-installed`, when `adopt scan` runs, the process
//! exits 1.
//!
//! See PRD-adopt-exit-code-and-docket-bridge.md.

use std::process::Command;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

#[test]
fn scan_exit_stale_exits_1() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-scanexit-ac3";

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

    // Installed binary FIRST (old mtime), then a src/ commit strictly after it,
    // so the binary is genuinely behind — installed-stale.
    let fake_bin = fake_bin_dir.join(bin_name);
    std::fs::write(&fake_bin, b"#!/bin/sh").expect("fake bin");
    let old_time = SystemTime::now() - Duration::from_secs(10 * 86400);
    let old_ft = filetime::FileTime::from_system_time(old_time);
    filetime::set_file_mtime(&fake_bin, old_ft).expect("set mtime");

    Command::new("git").args(["init"]).current_dir(&repo).output().expect("git init");
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
        .args(["scan", "--format", "json"])
        .env("WM_WINTERMUTE_DIR", &repos_dir)
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 for an installed-stale artifact with zero not-installed, got {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\n{stdout}"));

    // Zero not-installed among surviving artifacts.
    assert!(
        !arr.iter().any(|e| e["verdict"].as_str() == Some("not-installed")),
        "fixture must have zero not-installed artifacts:\n{stdout}"
    );
    let entry = arr
        .iter()
        .find(|e| e["bin"].as_str() == Some(bin_name))
        .unwrap_or_else(|| panic!("{bin_name} not found:\n{stdout}"));
    assert_eq!(entry["verdict"].as_str(), Some("installed-stale"));
}
