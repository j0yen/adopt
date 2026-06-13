//! AC1 (MUST): `adopt scan --format json` emits a JSON array; each element
//! has at minimum `repo`, `bin`, `verdict`, `is_daemon`, and `fix_cmd` keys.

use std::process::Command;

fn adopt_bin() -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

#[test]
fn scan_json_emits_array_with_required_keys() {
    // Build the binary first (in-process it's already built by cargo test).
    let out = Command::new(adopt_bin())
        .args(["scan", "--format", "json"])
        .output()
        .expect("failed to run adopt");

    assert!(out.status.success(), "adopt scan --format json exited non-zero: {:?}", out.status);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .expect("stdout was not valid JSON");

    let arr = val.as_array().expect("JSON output must be an array");

    // For each element verify required keys exist.
    for (i, elem) in arr.iter().enumerate() {
        let obj = elem.as_object().unwrap_or_else(|| panic!("element {i} is not an object"));
        for key in &["repo", "bin", "verdict", "is_daemon", "fix_cmd"] {
            assert!(obj.contains_key(*key), "element {i} missing key: {key}");
        }
    }
}
