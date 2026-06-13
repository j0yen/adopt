//! AC2 (MUST): A wintermute repo declaring a `[[bin]]` whose binary is absent
//! from PATH/~/.local/bin/~/.cargo/bin gets verdict `not-installed` and
//! fix_cmd of the form `cargo install --path <repo> --root ~/.local`.

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

/// Build a minimal Cargo project in a temp dir that won't be installed anywhere.
fn make_temp_bin_repo(tmp: &TempDir, bin_name: &str) -> std::path::PathBuf {
    let repo = tmp.path().join(bin_name);
    std::fs::create_dir_all(repo.join("src")).expect("create src dir");

    // Write a minimal Cargo.toml with a [[bin]].
    std::fs::write(
        repo.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{bin_name}"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "{bin_name}"
path = "src/main.rs"
"#
        ),
    )
    .expect("write Cargo.toml");
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}")
        .expect("write src/main.rs");

    // Init a git repo with a commit so git log -1 succeeds.
    Command::new("git")
        .args(["init"])
        .current_dir(&repo)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["-c", "user.email=t@t.com", "-c", "user.name=T", "commit", "--allow-empty", "-m", "init"])
        .current_dir(&repo)
        .output()
        .expect("git commit");

    repo
}

#[test]
fn absent_binary_reports_not_installed_with_fix_cmd() {
    let tmp = TempDir::new().expect("tempdir");
    // Use a unique bin name that is definitely not installed.
    let bin_name = "adopt-test-fixture-xyz999";
    let repo = make_temp_bin_repo(&tmp, bin_name);

    // Temporarily override HOME to point tests at the temp dir.
    // Actually we can't override HOME easily here. Instead scan the repo directly
    // by re-implementing the public API (unit test via library code).
    // We'll use the JSON output and grep for our fixture.
    // The scan walks ~/wintermute — our temp dir is not there, so we test via
    // the library unit directly.

    // Test the library layer directly.
    // We call find_installed and bins_from_cargo_toml via the scan module.
    // Since those are crate-private, drive this via the integration path:
    // set WM_WINTERMUTE_DIR to tmp.path() so the scan walks our fixture.
    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--all"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "non-zero: {:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON parse: {e}\nstdout: {stdout}"));

    let entry = arr.iter().find(|e| e["bin"].as_str() == Some(bin_name))
        .unwrap_or_else(|| panic!("fixture bin {bin_name} not found in output:\n{stdout}"));

    assert_eq!(entry["verdict"].as_str(), Some("not-installed"),
        "expected not-installed, got: {}", entry["verdict"]);

    let fix = entry["fix_cmd"].as_str().unwrap_or("");
    assert!(
        fix.contains("cargo install") && fix.contains("--root") && fix.contains(repo.to_str().unwrap_or("")),
        "fix_cmd should be `cargo install --path <repo> --root ~/.local`, got: {fix}"
    );
}
