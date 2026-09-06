//! AC1 (`scan_exit_actionable)`: Given a lineage with at least one
//! `not-installed` artifact, when `adopt scan --format json` runs, stdout is
//! valid JSON listing that verdict and the process exits 1.
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

/// A minimal Cargo bin project that is never installed anywhere.
fn make_bin_repo(dir: &std::path::Path, bin_name: &str) {
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
        .args(["-c", "user.email=t@t.com", "-c", "user.name=T", "commit", "--allow-empty", "-m", "init"])
        .current_dir(&repo)
        .output()
        .expect("git commit");
}

#[test]
fn scan_exit_actionable_not_installed_exits_1() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-scanexit-ac1";
    make_bin_repo(tmp.path(), bin_name);

    // Isolate from the real machine: fake HOME, minimal PATH — the fixture
    // binary is never installed anywhere reachable.
    let fake_home = tmp.path().join("fakehome");
    std::fs::create_dir_all(&fake_home).expect("fake home");

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .env("HOME", &fake_home)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run adopt");

    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 for a not-installed artifact, got {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\n{stdout}"));

    let entry = arr
        .iter()
        .find(|e| e["bin"].as_str() == Some(bin_name))
        .unwrap_or_else(|| panic!("{bin_name} not found in JSON output:\n{stdout}"));
    assert_eq!(
        entry["verdict"].as_str(),
        Some("not-installed"),
        "expected not-installed, got: {}",
        entry["verdict"]
    );
}
