//! Report AC2: each invocation carries a per-reason `--key adopt-stale-<reason>:<bin>`,
//! non-empty `--title`, `--evidence path:` and `--evidence commit:`.
//!
//! Updated for vest-verify: keys are now `adopt-stale-<reason>:<bin>` instead of
//! the old `adopt:<bin>` so the docket can escalate each failure category separately.

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

    // --key is now per-reason: adopt-stale-<reason>:<bin>
    let key_pos = args_str.iter().position(|a| *a == "--key").expect("--key missing");
    let key_val = args_str[key_pos + 1];
    assert!(
        key_val.starts_with("adopt-stale-") && key_val.ends_with(":rollout"),
        "--key must be adopt-stale-<reason>:rollout, got: {key_val}"
    );

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
