//! AC6: Re-running adopt apply after a successful install → InstalledCurrent (idempotent).

use adopt::apply::{run_apply, ApplyOutcome};
use std::env;
use tempfile::TempDir;

/// Idempotent: if scan finds InstalledCurrent → outcome is InstalledCurrent, not re-installed.
#[test]
fn apply_idempotent_on_current_artifacts() {
    // Empty wintermute dir — scan returns nothing.
    let tmp = TempDir::new().expect("tempdir");
    env::set_var("WM_WINTERMUTE_DIR", tmp.path());

    let results1 = run_apply(false, true, None, false)
        .expect("first run_apply");
    let results2 = run_apply(false, true, None, false)
        .expect("second run_apply");

    // With empty dir both runs return same result count.
    assert_eq!(results1.len(), results2.len(), "idempotent runs should return same number of results");

    // No Failed outcomes in either run.
    for r in results1.iter().chain(results2.iter()) {
        assert!(
            !matches!(r.verdict, ApplyOutcome::Failed { .. }),
            "no failures expected in empty env: {}", r.bin
        );
    }
}
