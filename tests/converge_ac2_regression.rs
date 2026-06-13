//! AC2 / AC3: convergence-check logic with synthetic ledger fixtures.
//!
//! These fixtures are written independently of the assertion logic in
//! `src/converge.rs` — they encode *what the pipeline should observe*
//! from the convergence contract, not how the code implements it.

use adopt::converge::{check_convergence, ConvergeRecord};

fn rec(run: &str, behind: u32) -> ConvergeRecord {
    ConvergeRecord {
        run: run.to_owned(),
        ts: "2026-06-13T00:00:00Z".to_owned(),
        total: 20,
        behind,
        dirty_blocked: 0,
        fallback: 0,
        lineage_current: 20 - behind,
    }
}

// ── AC2: increasing behind → alert ───────────────────────────────────────────

#[test]
fn ac2_alert_when_behind_increases() {
    // Synthetic held-out fixture: trend goes 2 → 5 (regression).
    let ledger = vec![rec("r1", 2), rec("r2", 5)];
    let alert = check_convergence(&ledger, 4);
    assert!(alert.is_some(), "must alert when behind increases");
    let a = alert.unwrap();
    assert!(!a.is_stall, "regression alert must not be marked as stall");
    assert_eq!(a.behind, 5);
}

#[test]
fn ac2_no_alert_when_behind_decreasing() {
    // Trend 8 → 5 → 3 → 1: converging, no alert.
    let ledger = vec![rec("r1", 8), rec("r2", 5), rec("r3", 3), rec("r4", 1)];
    assert!(check_convergence(&ledger, 4).is_none());
}

// ── AC3: behind reaches 0 → no alert ─────────────────────────────────────────

#[test]
fn ac3_no_alert_when_behind_zero() {
    // Fixture: pipeline converged in final run.
    let ledger = vec![rec("r1", 4), rec("r2", 2), rec("r3", 0)];
    assert!(
        check_convergence(&ledger, 4).is_none(),
        "no alert once behind reaches 0"
    );
}

#[test]
fn ac3_still_no_alert_if_last_is_zero_despite_earlier_regression() {
    // Even if behind was increasing, if the last run hit 0 → no alert.
    let ledger = vec![rec("r1", 1), rec("r2", 5), rec("r3", 0)];
    assert!(check_convergence(&ledger, 4).is_none());
}

// ── AC4: stall rule ───────────────────────────────────────────────────────────

#[test]
fn ac4_stall_fires_after_n_consecutive_nonzero_runs() {
    // Exactly 4 consecutive runs with behind > 0 at stall_runs = 4.
    let ledger = vec![rec("r1", 3), rec("r2", 3), rec("r3", 3), rec("r4", 3)];
    let alert = check_convergence(&ledger, 4);
    assert!(alert.is_some(), "stall must fire after 4 consecutive non-zero runs");
    let a = alert.unwrap();
    assert!(a.is_stall, "alert must be classified as stall");
}

#[test]
fn ac4_stall_does_not_fire_below_threshold() {
    // 3 consecutive runs with behind > 0, threshold = 4 → no stall yet.
    let ledger = vec![rec("r1", 3), rec("r2", 3), rec("r3", 3)];
    assert!(
        check_convergence(&ledger, 4).is_none(),
        "stall must not fire when tail is shorter than stall_runs"
    );
}

#[test]
fn ac4_stall_clears_when_behind_hits_zero() {
    // 5 non-zero runs followed by 0 → clears.
    let ledger = vec![
        rec("r1", 2),
        rec("r2", 2),
        rec("r3", 2),
        rec("r4", 2),
        rec("r5", 2),
        rec("r6", 0),
    ];
    assert!(
        check_convergence(&ledger, 4).is_none(),
        "stall must clear when behind reaches 0"
    );
}

// ── Edge cases ────────────────────────────────────────────────────────────────

#[test]
fn single_record_produces_no_alert() {
    // Not enough history for regression or stall detection.
    let ledger = vec![rec("r1", 7)];
    assert!(check_convergence(&ledger, 4).is_none());
}

#[test]
fn empty_ledger_produces_no_alert() {
    assert!(check_convergence(&[], 4).is_none());
}

#[test]
fn stall_runs_zero_disables_stall_check() {
    // stall_runs = 0 → stall check disabled.
    let ledger = vec![rec("r1", 5), rec("r2", 5)];
    assert!(check_convergence(&ledger, 0).is_none());
}
