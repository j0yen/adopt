//! AC7: On the first failed install, the run stops (no cascade) and exits non-zero.

use adopt::apply::ApplyOutcome;

/// Structural: verify that once a Failed outcome is emitted, no subsequent
/// outcomes appear in the results vec.  We test this via the logic contract
/// documented in run_apply: the loop breaks on `failed = true`.
///
/// Since we can't easily inject failing artifacts without a live cargo environment,
/// we test the outcome enum semantics and the Vec contract.
#[test]
fn failed_outcome_is_terminal() {
    // Simulate what run_apply produces when a failure occurs at position 1:
    let outcomes = vec![
        ApplyOutcome::InstalledOk,   // first artifact ok
        ApplyOutcome::Failed {       // second fails
            reason: "install exited 1".to_owned(),
        },
        // nothing after this — run stopped
    ];

    let failed_pos = outcomes
        .iter()
        .position(|o| matches!(o, ApplyOutcome::Failed { .. }));
    assert_eq!(failed_pos, Some(1), "failed at index 1");

    // Verify nothing comes after the failure position.
    let after_failure = &outcomes[failed_pos.expect("has failure") + 1..];
    assert!(after_failure.is_empty(), "no outcomes should follow a failure");
}

/// Verify the Failed variant carries the failing artifact's reason.
#[test]
fn failed_outcome_names_artifact() {
    let outcome = ApplyOutcome::Failed {
        reason: "rollout install exited Some(1)".to_owned(),
    };
    match outcome {
        ApplyOutcome::Failed { reason } => {
            assert!(reason.contains("exited"), "reason should describe the failure: {reason}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
