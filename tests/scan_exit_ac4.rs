//! AC4 (`scan_exit_match_filter)`: Given one `not-installed` artifact named
//! `foo` and the rest current, when `adopt scan --match 'bar*'` runs (which
//! excludes `foo`), the process exits 0 — the exit code reflects only the
//! artifacts that survive the filter.
//!
//! See PRD-adopt-exit-code-and-docket-bridge.md.

use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

fn make_bin_repo(dir: &std::path::Path, bin_name: &str) -> std::path::PathBuf {
    let repo = dir.join(bin_name);
    std::fs::create_dir_all(repo.join("src")).expect("src dir");
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
    repo
}

#[test]
fn scan_exit_match_filter_excludes_actionable_exits_0() {
    let tmp = TempDir::new().expect("tempdir");
    let repos_dir = tmp.path().join("repos");
    let fake_bin_dir = tmp.path().join("local").join("bin");
    std::fs::create_dir_all(&fake_bin_dir).expect("bin dir");

    // "foo": never installed (not-installed) — must be filtered OUT by --match.
    make_bin_repo(&repos_dir, "foo");

    // "bar": installed and current — must survive the filter.
    let bar_repo = make_bin_repo(&repos_dir, "bar");
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(fake_bin_dir.join("bar"), b"#!/bin/sh\n").expect("fake bar bin");
    let _ = bar_repo; // silence unused warning if repo path unused further

    let fake_home = tmp.path().join("fakehome");
    std::fs::create_dir_all(&fake_home).expect("fake home");
    let new_path = format!("{}:/usr/bin:/bin", fake_bin_dir.display());

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--match", "bar*"])
        .env("WM_WINTERMUTE_DIR", &repos_dir)
        .env("HOME", &fake_home)
        .env("PATH", &new_path)
        .output()
        .expect("run adopt");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\n{stdout}"));

    // "foo" must not appear — it was filtered out by --match.
    assert!(
        !arr.iter().any(|e| e["bin"].as_str() == Some("foo")),
        "foo should have been excluded by --match 'bar*':\n{stdout}"
    );

    assert!(
        out.status.success(),
        "expected exit 0 once the only not-installed artifact is filtered out, got {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
