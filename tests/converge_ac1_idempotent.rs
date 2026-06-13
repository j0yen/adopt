//! AC1: append_record is idempotent per run id.
//!
//! Appending the same run id twice must not produce a duplicate JSONL line.

use adopt::converge::{append_record, read_records, ConvergeRecord};
use tempfile::TempDir;

fn make_record(run: &str, behind: u32) -> ConvergeRecord {
    ConvergeRecord {
        run: run.to_owned(),
        ts: "2026-06-13T00:00:00Z".to_owned(),
        total: 10,
        behind,
        dirty_blocked: 0,
        fallback: 1,
        lineage_current: 9,
    }
}

#[test]
fn append_same_run_id_does_not_duplicate() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("converge.jsonl");

    let rec = make_record("2026-06-13.1", 3);

    // First append.
    append_record(&rec, &path).expect("first append");
    let after_first = read_records(&path, None).expect("read after first");
    assert_eq!(after_first.len(), 1, "should have 1 record after first append");

    // Second append with same run id — must be a no-op.
    append_record(&rec, &path).expect("second append");
    let after_second = read_records(&path, None).expect("read after second");
    assert_eq!(
        after_second.len(),
        1,
        "idempotent: same run id must not produce a second line"
    );
}

#[test]
fn different_run_ids_produce_separate_lines() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("converge.jsonl");

    append_record(&make_record("run-1", 5), &path).expect("append run-1");
    append_record(&make_record("run-2", 3), &path).expect("append run-2");
    append_record(&make_record("run-1", 5), &path).expect("re-append run-1 (no-op)");

    let records = read_records(&path, None).expect("read");
    assert_eq!(records.len(), 2, "two distinct run ids → two lines");
    assert_eq!(records[0].run, "run-1");
    assert_eq!(records[1].run, "run-2");
}

#[test]
fn read_limit_returns_last_n() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("converge.jsonl");

    for i in 1_u32..=6 {
        let rec = ConvergeRecord {
            run: format!("r{i}"),
            ts: "2026-06-13T00:00:00Z".to_owned(),
            total: 10,
            behind: i,
            dirty_blocked: 0,
            fallback: 0,
            lineage_current: 10 - i,
        };
        append_record(&rec, &path).expect("append");
    }

    let last3 = read_records(&path, Some(3)).expect("read last 3");
    assert_eq!(last3.len(), 3);
    assert_eq!(last3[0].run, "r4");
    assert_eq!(last3[2].run, "r6");
}

#[test]
fn read_records_on_missing_file_returns_empty() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("nonexistent.jsonl");

    let records = read_records(&path, None).expect("no error on missing file");
    assert!(records.is_empty());
}

#[test]
fn record_fields_round_trip_json() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("converge.jsonl");

    let original = ConvergeRecord {
        run: "2026-06-13.rt".to_owned(),
        ts: "2026-06-13T12:34:56Z".to_owned(),
        total: 42,
        behind: 7,
        dirty_blocked: 2,
        fallback: 3,
        lineage_current: 30,
    };

    append_record(&original, &path).expect("append");
    let records = read_records(&path, None).expect("read");
    assert_eq!(records.len(), 1);

    let got = &records[0];
    assert_eq!(got.run, original.run);
    assert_eq!(got.ts, original.ts);
    assert_eq!(got.total, original.total);
    assert_eq!(got.behind, original.behind);
    assert_eq!(got.dirty_blocked, original.dirty_blocked);
    assert_eq!(got.fallback, original.fallback);
    assert_eq!(got.lineage_current, original.lineage_current);
}
