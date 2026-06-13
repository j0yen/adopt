//! Integration tests for scion-truth: lineage-based docket reporting.
//!
//! AC1: lineage-behind count == 0 → adopt-scan-stale-binaries resolved
//! AC2: lineage-behind count == 1 → finding stays open (report emitted)
//! AC3: clock-fallback stale → adopt-unmarked-installs, NOT in behind count
//! AC4: --format json includes per-artifact freshness_basis
//! AC5: resolve path uses same docket mechanism as reports

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use adopt::types::{ArtifactResult, FreshnessBasis, Verdict};

fn adopt_bin() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = base.join("target/release/adopt");
    if release.exists() {
        return release;
    }
    base.join("target/debug/adopt")
}

fn make_artifact(
    bin: &str,
    verdict: Verdict,
    freshness_basis: FreshnessBasis,
    is_daemon: bool,
    repo: &str,
) -> ArtifactResult {
    ArtifactResult {
        repo: repo.to_owned(),
        bin: bin.to_owned(),
        verdict,
        installed_path: None,
        is_daemon,
        source_commit_ts: Some(2_000_000_000),
        installed_ts: Some(1_900_000_000),
        fix_cmd: String::new(),
        age_vs_head: None,
        freshness_basis,
    }
}

fn mock_docket_dir(record: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("docket");
    let rec_str = record.to_string_lossy().replace('\'', "'\\''");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{rec_str}'\n");
    fs::write(&script_path, &script).expect("write mock docket");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod mock docket");
    }
    dir
}

/// Write artifacts JSON to a temp file and return the path.
fn write_artifacts_json(dir: &tempfile::TempDir, artifacts: &[ArtifactResult]) -> PathBuf {
    let path = dir.path().join("artifacts.json");
    fs::write(&path, serde_json::to_string(artifacts).expect("serialize artifacts"))
        .expect("write artifacts.json");
    path
}

/// Run `adopt report` with given args and return (success, stdout, docket_calls).
fn run_report(
    json_path: &std::path::Path,
    mock_dir: &tempfile::TempDir,
    record: &std::path::Path,
    extra_args: &[&str],
) -> (bool, String, String) {
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{orig_path}", mock_dir.path().display());

    let mut cmd_args = vec![
        "report",
        "--run",
        "scion-truth-test",
        "--from-json",
        json_path.to_str().expect("json path"),
    ];
    cmd_args.extend_from_slice(extra_args);

    let output = Command::new(adopt_bin())
        .args(&cmd_args)
        .env("PATH", &new_path)
        .output()
        .expect("run adopt report");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let calls = if record.exists() {
        fs::read_to_string(record).unwrap_or_default()
    } else {
        String::new()
    };

    (output.status.success(), stdout, calls)
}

// ── AC1: All installs marker-current → count == 0 → resolve emitted ──────────

/// AC1: When all artifacts are installed-current (or not-a-bin), lineage-behind
/// count is 0 and adopt-scan-stale-binaries resolve marker is emitted.
#[test]
fn ac1_zero_lineage_behind_emits_resolve() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    let artifacts = vec![
        make_artifact(
            "wm-stt",
            Verdict::InstalledCurrent,
            FreshnessBasis::Lineage,
            false,
            "/wm/wm-stt",
        ),
        make_artifact(
            "libonly",
            Verdict::NotABin,
            FreshnessBasis::Lineage,
            false,
            "/wm/libonly",
        ),
    ];
    let json_path = write_artifacts_json(&tmp, &artifacts);

    let (ok, _stdout, calls) = run_report(&json_path, &mock_dir, &record, &[]);

    assert!(ok, "adopt report should succeed");

    // A resolve line must be present.
    let resolve_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("resolve"))
        .collect();
    assert!(!resolve_lines.is_empty(), "expected docket resolve, got calls: {calls}");

    // The resolve must mention the adopt-scan-stale-binaries slug.
    let resolve_mentions_slug = resolve_lines.iter().any(|l| l.contains("adopt-scan-stale-binaries"));
    assert!(
        resolve_mentions_slug,
        "resolve must target adopt-scan-stale-binaries, got: {resolve_lines:?}"
    );

    // No "report" subcommand lines.
    let report_lines: Vec<&str> = calls.lines().filter(|l| l.starts_with("report")).collect();
    assert!(
        report_lines.is_empty(),
        "unexpected docket report calls: {report_lines:?}"
    );
}

// ── AC2: One genuinely-behind artifact → count == 1 → finding stays open ─────

/// AC2: A single lineage-stale artifact produces a lineage-behind count of 1;
/// the docket finding (adopt-scan-stale-binaries) stays open (report emitted,
/// no resolve).
#[test]
fn ac2_one_lineage_behind_finding_stays_open() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    let artifacts = vec![make_artifact(
        "wm-brain",
        Verdict::InstalledStale,
        FreshnessBasis::Lineage,
        false,
        "/wm/wm-brain",
    )];
    let json_path = write_artifacts_json(&tmp, &artifacts);

    let (ok, _stdout, calls) = run_report(&json_path, &mock_dir, &record, &[]);

    assert!(ok, "adopt report should succeed");

    // A "report" docket call must be present.
    let report_lines: Vec<&str> = calls.lines().filter(|l| l.starts_with("report")).collect();
    assert!(!report_lines.is_empty(), "expected docket report for lineage-stale, got: {calls}");

    // The report key must contain the stale-binaries slug.
    let has_stale_key = report_lines
        .iter()
        .any(|l| l.contains("adopt-scan-stale-binaries"));
    assert!(
        has_stale_key,
        "report key must contain adopt-scan-stale-binaries, got: {report_lines:?}"
    );

    // No resolve must be emitted (finding stays open).
    let resolve_lines: Vec<&str> = calls.lines().filter(|l| l.starts_with("resolve")).collect();
    assert!(
        resolve_lines.is_empty(),
        "resolve must NOT be emitted when lineage-behind count > 0, got: {resolve_lines:?}"
    );
}

// ── AC3: Clock-fallback stale → adopt-unmarked-installs, not in behind count ──

/// AC3: Artifacts on clock-fallback are reported under adopt-unmarked-installs
/// and never counted in adopt-scan-stale-binaries.
#[test]
fn ac3_clock_fallback_goes_to_unmarked_bucket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    let artifacts = vec![make_artifact(
        "wm-tts",
        Verdict::InstalledStale,
        FreshnessBasis::ClockFallback,
        false,
        "/wm/wm-tts",
    )];
    let json_path = write_artifacts_json(&tmp, &artifacts);

    let (ok, _stdout, calls) = run_report(&json_path, &mock_dir, &record, &[]);

    assert!(ok, "adopt report should succeed");

    let report_lines: Vec<&str> = calls.lines().filter(|l| l.starts_with("report")).collect();

    // The report must target adopt-unmarked-installs.
    let has_unmarked = report_lines
        .iter()
        .any(|l| l.contains("adopt-unmarked-installs"));
    assert!(
        has_unmarked,
        "clock-fallback must go to adopt-unmarked-installs, got: {report_lines:?}"
    );

    // Must NOT be reported under adopt-scan-stale-binaries.
    let has_stale = report_lines
        .iter()
        .any(|l| l.contains("adopt-scan-stale-binaries"));
    assert!(
        !has_stale,
        "clock-fallback must NOT appear in adopt-scan-stale-binaries, got: {report_lines:?}"
    );

    // A resolve IS emitted (lineage count == 0).
    let has_resolve = calls.lines().any(|l| l.starts_with("resolve"));
    assert!(has_resolve, "resolve must be emitted when lineage count == 0, got: {calls}");
}

/// AC3b: clock-fallback and lineage-stale are disjoint — they never appear in
/// the same bucket.
#[test]
fn ac3b_buckets_are_disjoint() {
    use adopt::report::partition_artifacts;

    let artifacts = vec![
        make_artifact(
            "lineage-bin",
            Verdict::InstalledStale,
            FreshnessBasis::Lineage,
            false,
            "/wm/l",
        ),
        make_artifact(
            "clock-bin",
            Verdict::InstalledStale,
            FreshnessBasis::ClockFallback,
            false,
            "/wm/c",
        ),
        make_artifact(
            "missing-bin",
            Verdict::NotInstalled,
            FreshnessBasis::ClockFallback,
            false,
            "/wm/m",
        ),
    ];

    let (lineage, clock, not_installed) = partition_artifacts(&artifacts);

    assert_eq!(lineage.len(), 1, "exactly 1 lineage-stale");
    assert_eq!(lineage[0].bin, "lineage-bin");

    assert_eq!(clock.len(), 1, "exactly 1 clock-fallback");
    assert_eq!(clock[0].bin, "clock-bin");

    assert_eq!(not_installed.len(), 1, "exactly 1 not-installed");
    assert_eq!(not_installed[0].bin, "missing-bin");
}

// ── AC4: --format json includes per-artifact freshness_basis ──────────────────

/// AC4: `adopt report --format json` includes `freshness_basis` under each finding.
#[test]
fn ac4_json_format_includes_freshness_basis() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    let artifacts = vec![
        make_artifact(
            "lin-bin",
            Verdict::InstalledStale,
            FreshnessBasis::Lineage,
            false,
            "/wm/lin",
        ),
        make_artifact(
            "clk-bin",
            Verdict::InstalledStale,
            FreshnessBasis::ClockFallback,
            false,
            "/wm/clk",
        ),
        make_artifact(
            "miss-bin",
            Verdict::NotInstalled,
            FreshnessBasis::ClockFallback,
            false,
            "/wm/miss",
        ),
    ];
    let json_path = write_artifacts_json(&tmp, &artifacts);

    let (ok, stdout, _calls) = run_report(&json_path, &mock_dir, &record, &["--format", "json"]);

    assert!(ok, "adopt report --format json should succeed");

    // The JSON output must be parseable.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("adopt report --format json produced invalid JSON");

    // Top-level must have lineage_stale, clock_fallback, not_installed.
    assert!(
        parsed.get("lineage_stale").is_some(),
        "JSON must have lineage_stale key"
    );
    assert!(
        parsed.get("clock_fallback").is_some(),
        "JSON must have clock_fallback key"
    );
    assert!(
        parsed.get("not_installed").is_some(),
        "JSON must have not_installed key"
    );

    // Each artifact entry must include freshness_basis.
    let lineage_arr = parsed["lineage_stale"].as_array().expect("lineage_stale is array");
    assert_eq!(lineage_arr.len(), 1, "one lineage-stale entry");
    assert!(
        lineage_arr[0].get("freshness_basis").is_some(),
        "lineage_stale entry must have freshness_basis"
    );
    assert_eq!(
        lineage_arr[0]["freshness_basis"].as_str().unwrap_or(""),
        "lineage",
        "freshness_basis must be 'lineage'"
    );

    let clock_arr = parsed["clock_fallback"].as_array().expect("clock_fallback is array");
    assert_eq!(clock_arr.len(), 1, "one clock-fallback entry");
    assert!(
        clock_arr[0].get("freshness_basis").is_some(),
        "clock_fallback entry must have freshness_basis"
    );
    assert_eq!(
        clock_arr[0]["freshness_basis"].as_str().unwrap_or(""),
        "clock-fallback",
        "freshness_basis must be 'clock-fallback'"
    );
}

// ── AC5: Resolve path uses same docket mechanism as reports ───────────────────

/// AC5: The resolve marker is emitted via the same docket subprocess mechanism
/// as report calls — verified by asserting the resolve appears in the mock
/// docket's call record (same file as report calls).
#[test]
fn ac5_resolve_uses_docket_mechanism() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = tmp.path().join("calls.txt");
    let mock_dir = mock_docket_dir(&record);

    // No lineage-stale artifacts → resolve should be emitted via docket.
    let artifacts: Vec<ArtifactResult> = vec![];
    let json_path = write_artifacts_json(&tmp, &artifacts);

    let (ok, _stdout, calls) = run_report(&json_path, &mock_dir, &record, &[]);

    assert!(ok, "adopt report should succeed with empty artifact list");

    // The resolve must appear in the mock docket's call log.
    assert!(
        record.exists(),
        "mock docket was not invoked (record file missing)"
    );
    let has_resolve = calls.lines().any(|l| l.starts_with("resolve"));
    assert!(
        has_resolve,
        "docket resolve must be recorded in mock call log, got: {calls}"
    );

    // build_resolve_args must produce valid args (unit-level check).
    let resolve_args = adopt::report::build_resolve_args("test-run", "adopt-scan-stale-binaries");
    let first = resolve_args.first().map(String::as_str).unwrap_or("");
    assert_eq!(first, "resolve", "first arg must be 'resolve'");
    let has_key_arg = resolve_args.windows(2).any(|w| w[0] == "--key");
    assert!(has_key_arg, "resolve args must include --key");
}
