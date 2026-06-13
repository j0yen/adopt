//! Report AC2: each invocation carries `--key adopt:<bin>`, non-empty `--title`,
//! `--evidence path:` and `--evidence commit:`.

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
fn invocation_carries_required_flags() {
    let artifact = make_artifact("rollout", Verdict::NotInstalled, false, "/home/jsy/wintermute/rollout");
    let args = build_docket_args("run-x", &artifact);
    let args_str: Vec<&str> = args.iter().map(String::as_str).collect();

    // --key adopt:<bin>
    let key_pos = args_str.iter().position(|a| *a == "--key").expect("--key missing");
    assert_eq!(args_str[key_pos + 1], "adopt:rollout", "--key must be adopt:<bin>");

    // --title non-empty
    let title_pos = args_str.iter().position(|a| *a == "--title").expect("--title missing");
    assert!(!args_str[title_pos + 1].is_empty(), "--title must be non-empty");

    // --evidence path:
    let has_path = args_str
        .windows(2)
        .any(|w| w[0] == "--evidence" && w[1].starts_with("path:"));
    assert!(has_path, "missing --evidence path:");

    // --evidence commit:
    let has_commit = args_str
        .windows(2)
        .any(|w| w[0] == "--evidence" && w[1].starts_with("commit:"));
    assert!(has_commit, "missing --evidence commit:");
}
