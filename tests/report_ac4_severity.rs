//! Report AC4: daemon artifact → `--severity error`; plain CLI → `--severity warn`.

use adopt::report::build_docket_args;
use adopt::types::{ArtifactResult, FreshnessBasis, Verdict};

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
        freshness_basis: FreshnessBasis::ClockFallback,
    }
}

#[test]
fn daemon_severity_error_cli_severity_warn() {
    let daemon_args = build_docket_args(
        "run-y",
        &make_artifact("wmd-init", Verdict::NotInstalled, true, "/wm/wmd-init"),
    );
    let cli_args = build_docket_args(
        "run-y",
        &make_artifact("rollout", Verdict::NotInstalled, false, "/wm/rollout"),
    );

    let sev_pos_d = daemon_args
        .iter()
        .position(|a| a == "--severity")
        .expect("--severity missing (daemon)");
    assert_eq!(daemon_args[sev_pos_d + 1], "error", "daemon must be severity=error");

    let sev_pos_c = cli_args
        .iter()
        .position(|a| a == "--severity")
        .expect("--severity missing (cli)");
    assert_eq!(cli_args[sev_pos_c + 1], "warn", "plain CLI must be severity=warn");
}
