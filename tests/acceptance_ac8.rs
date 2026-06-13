//! AC8 (MUST): `adopt scan --match '^wm-'` restricts output to artifacts
//! whose bin name starts with `wm-`.

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

fn make_bin_repo(dir: &std::path::Path, bin_name: &str) {
    let repo = dir.join(bin_name);
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(
        repo.join("Cargo.toml"),
        format!("[package]\nname = \"{bin_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"{bin_name}\"\npath = \"src/main.rs\"\n"),
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
fn match_regex_filters_output() {
    let tmp = TempDir::new().expect("tempdir");
    make_bin_repo(tmp.path(), "wm-audio");
    make_bin_repo(tmp.path(), "wm-stt");
    make_bin_repo(tmp.path(), "other-tool");

    // Use a fake HOME and minimal PATH so real installed binaries don't interfere.
    let fake_home = tmp.path().join("fakehome");
    std::fs::create_dir_all(&fake_home).expect("fake home");

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json", "--match", "^wm-"])
        .env("WM_WINTERMUTE_DIR", tmp.path())
        .env("HOME", &fake_home)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "{:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));

    // Every returned bin must start with `wm-`.
    for entry in &arr {
        let bin = entry["bin"].as_str().unwrap_or("");
        assert!(
            bin.starts_with("wm-"),
            "entry {bin:?} does not match ^wm- filter"
        );
    }

    // `other-tool` must not appear.
    let has_other = arr.iter().any(|e| e["bin"].as_str() == Some("other-tool"));
    assert!(!has_other, "other-tool should have been filtered out");

    // wm-audio and wm-stt should appear (they're not installed → not-installed).
    let bins: Vec<&str> = arr.iter().filter_map(|e| e["bin"].as_str()).collect();
    assert!(bins.contains(&"wm-audio"), "wm-audio should appear");
    assert!(bins.contains(&"wm-stt"), "wm-stt should appear");
}
