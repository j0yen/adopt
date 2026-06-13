//! Report AC8: artifact-derived strings with shell metacharacters are passed as
//! discrete args, not interpolated into a shell string.

use adopt::report::build_docket_args;
use adopt::types::{ArtifactResult, Verdict};

fn make_artifact(bin: &str, verdict: Verdict, is_daemon: bool, repo: &str) -> ArtifactResult {
    ArtifactResult {
        repo: repo.to_owned(),
        bin: bin.to_owned(),
        verdict,
        installed_path: None,
        is_daemon,
        source_commit_ts: None,
        installed_ts: None,
        fix_cmd: String::new(),
        age_vs_head: None,
    }
}

#[test]
fn metacharacter_in_repo_preserved_literally() {
    let nasty_repo = "/home/jsy/wintermute/my repo $(evil)";
    let artifact = make_artifact("evilbin", Verdict::NotInstalled, false, nasty_repo);
    let args = build_docket_args("run-ac8", &artifact);

    // Find the --evidence path: value.
    let evidence_val = args
        .windows(2)
        .find(|w| w[0] == "--evidence" && w[1].starts_with("path:"))
        .map(|w| w[1].as_str())
        .expect("--evidence path: missing");

    assert!(
        evidence_val.starts_with("path:"),
        "evidence must be path: prefixed"
    );
    assert!(
        evidence_val.contains(nasty_repo),
        "literal metacharacter path not preserved: got {evidence_val}"
    );

    // Verify `--key` also contains the raw bin name (no quoting).
    let key_val = args
        .windows(2)
        .find(|w| w[0] == "--key")
        .map(|w| w[1].as_str())
        .expect("--key missing");
    assert_eq!(key_val, "adopt:evilbin");
}
