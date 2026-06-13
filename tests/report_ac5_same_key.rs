//! Report AC5: same key/run pair produces identical args (docket handles dedupe).

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
fn same_run_same_key_args_are_deterministic() {
    let artifact = make_artifact("rollout", Verdict::NotInstalled, false, "/wm/rollout");
    let args1 = build_docket_args("run-z", &artifact);
    let args2 = build_docket_args("run-z", &artifact);
    assert_eq!(args1, args2, "args must be deterministic for same run/artifact");

    // Verify key value follows per-reason format: adopt-stale-<reason>:<bin>.
    let key_pos = args1.iter().position(|a| a == "--key").expect("--key missing");
    let key_val = &args1[key_pos + 1];
    assert!(
        key_val.starts_with("adopt-stale-") && key_val.ends_with(":rollout"),
        "--key must be adopt-stale-<reason>:rollout, got: {key_val}"
    );
}
