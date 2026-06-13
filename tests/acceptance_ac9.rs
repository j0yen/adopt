//! AC9 (MUST): When binstale is on PATH and artifact is a daemon, is_daemon: true.
//! When binstale is absent, scan still completes.

use std::process::Command;
use tempfile::TempDir;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() { return release; }
    base.join("target/debug/adopt")
}

fn make_bin_repo_with_systemd_unit(tmp: &TempDir, bin_name: &str) -> std::path::PathBuf {
    let repos_dir = tmp.path().join("repos");
    let repo = repos_dir.join(bin_name);
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

    // Write a systemd user unit referencing this binary.
    let systemd_dir = tmp.path().join("systemd").join("user");
    std::fs::create_dir_all(&systemd_dir).expect("systemd dir");
    std::fs::write(
        systemd_dir.join(format!("{bin_name}.service")),
        format!(
            "[Unit]\nDescription={bin_name}\n\n[Service]\nExecStart=/home/user/.local/bin/{bin_name}\n\n[Install]\nWantedBy=default.target\n"
        ),
    )
    .expect("service file");

    repos_dir
}

/// AC9a: scan completes even with no binstale on PATH.
#[test]
fn scan_completes_without_binstale() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-ac9-no-binstale";
    let repos_dir = make_bin_repo_with_systemd_unit(&tmp, bin_name);

    // Remove binstale from PATH by using a stripped PATH.
    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .env("WM_WINTERMUTE_DIR", &repos_dir)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run adopt");

    assert!(
        out.status.success(),
        "scan should complete without binstale: {:?}",
        out.status
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let _arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));
}

/// AC9b: artifact backed by systemd unit gets is_daemon: true.
/// We test this by overriding the XDG_CONFIG_HOME to point to our
/// temp systemd dir.
#[test]
fn daemon_artifact_gets_is_daemon_true() {
    let tmp = TempDir::new().expect("tempdir");
    let bin_name = "adopt-test-ac9-daemon";
    let repos_dir = make_bin_repo_with_systemd_unit(&tmp, bin_name);

    // The systemd unit file is at tmp/systemd/user/*.service.
    // adopt checks ~/.config/systemd/user/ — we override HOME.
    let fake_home = tmp.path().join("fakehome");
    let systemd_user = fake_home.join(".config/systemd/user");
    std::fs::create_dir_all(&systemd_user).expect("config/systemd/user");

    // Copy the service file into fake home.
    let service_src = tmp.path().join("systemd/user").join(format!("{bin_name}.service"));
    let service_dst = systemd_user.join(format!("{bin_name}.service"));
    std::fs::copy(&service_src, &service_dst).expect("copy service");

    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .env("WM_WINTERMUTE_DIR", &repos_dir)
        .env("HOME", &fake_home)
        .output()
        .expect("run adopt");

    assert!(out.status.success(), "{:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON: {e}\n{stdout}"));

    let entry = arr.iter().find(|e| e["bin"].as_str() == Some(bin_name))
        .unwrap_or_else(|| panic!("{bin_name} not found:\n{stdout}"));

    assert_eq!(
        entry["is_daemon"].as_bool(),
        Some(true),
        "artifact backed by systemd unit should have is_daemon: true"
    );
}
