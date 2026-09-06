# PRD: adopt-exit-code-and-docket-bridge — scan tells the truth in its exit code and the docket bridge speaks docket's CLI

- Status: building
- Lane: RedBaron 2026-09-06T07:51:40Z
- Blocked: 2026-09-06 follow-up — added `.github/workflows/ci.yml` (build + test --release + clippy, no `-D warnings` yet) to j0yen/adopt to close blocker (1) ci-checks (run_count=0, no workflow existed); expected to resolve on the next extend-gate run once GitHub has executed it. Still open, unresolved by this step: (2) reviewer-agent decision=block — `cargo clippy --workspace -- -D warnings` has 67 pre-existing errors and `cargo deny check advisories` flags RUSTSEC-2026-0190 (anyhow 1.0.102), neither from this PRD's diff, both larger separate cleanups; (3) ac-traceability verdict=block — no PRD-*.md at repo root, extended-gates.toml `prd_path` not wired for this repo; (4) intake and (5) risk-gate receipts still missing (repo never onboarded). Code fix + tests for this PRD's own scope remain green (`cargo test --release`: 38/38 suites pass).
- build_target: rust-extend
- build_into: /home/jsy/wintermute/adopt
- build_priority: normal
- build_version_bump: patch
- test_prefix: [scan_exit, docket_bridge]
- publish: j0yen/private
- Vision: visions/buildloop-operations.md
- PM: Joe
- Drafted: 2026-09-04
- Engineering target: j0yen/adopt `src/cli.rs` (Scan arm), `src/report.rs` (`build_resolve_args`), tests

## TL;DR

`adopt scan` exits 0 even when artifacts are not installed, so every caller that trusts the exit code — the self-review `adopt_scan_probe` playbook first among them — reads "all current" on a box with 22 unadopted artifacts. Separately, `adopt report` shells out to `docket resolve` with `--run`/`--key` flags that docket rejects (exit 2), so resolved findings never close in the ledger. Two small fixes in one patch release.

## Problem statement

The self-review skill's `adopt_scan_probe` playbook branches on `adopt scan`'s exit code: 0 means "all artifacts current", 1 means "at least one not-installed or installed-stale". The Scan arm in `src/cli.rs:160-165` prints results and returns `Ok(())` unconditionally, unlike the Apply (`cli.rs:174-182`) and Verify (`cli.rs:207-213`) arms, which `bail!()` on actionable items. The 2026-09-04 self-review observed exit 0 alongside 22 `not-installed` verdicts; the 2026-08-31 review logged the same anomaly.

`build_resolve_args` in `src/report.rs:140-147` constructs `docket resolve --run <run_id> --key <slug>`. Docket's actual signature is `docket resolve [OPTIONS] <KEY>` with an optional `--reason` — no `--run`, no `--key`. Every resolve attempt dies with clap's usage error and `adopt: docket exited with status exit status: 2`, aborting the rest of the report pass.

## Goals

- `adopt scan` exit code matches the printed verdicts: 1 when anything is actionable, 0 only when everything is `installed-current` (or `not-a-bin`).
- `adopt report` drives docket's real CLI for resolve calls and completes a full pass against a live docket.

## Non-goals

- No change to verdict computation, table/JSON output shape, or the `Verdict` enum serde names.
- No change to `docket report` invocations that already work.
- No new subcommands or flags.

## Requirements

P0 — the Scan arm computes an actionable count (`not-installed` + `installed-stale`) after printing and exits 1 when it is nonzero, mirroring the Apply/Verify pattern. Stdout stays byte-identical so `--format json` consumers keep parsing.

P0 — `build_resolve_args` returns `["resolve", <slug>]`, optionally followed by `["--reason", <reason>]` when a reason is supplied. The `run_id` no longer appears in resolve argv anywhere.

P1 — a report pass that hits a docket CLI error logs the failed argv verbatim before the exit-status line, so the next CLI drift is diagnosable from the error message alone.

Edge case — `scan --match <pattern>` filtering: the exit code reflects only the artifacts that survive the filter.

## Success metrics

- Next self-review run: `adopt scan` exit code disagrees with the JSON verdict list zero times.
- `adopt report --run <id>` completes without a docket usage error and previously-resolved `adopt:*` findings close in `docket list --open`.

## Open questions

None.

## Acceptance criteria

1. AC1 scan_exit_actionable: Given a lineage with at least one `not-installed` artifact, When `adopt scan --format json` runs, Then stdout is valid JSON listing that verdict And the process exits 1.
2. AC2 scan_exit_clean: Given a lineage where every artifact is `installed-current` or `not-a-bin`, When `adopt scan` runs, Then the process exits 0.
3. AC3 scan_exit_stale: Given a lineage with at least one `installed-stale` artifact and zero `not-installed`, When `adopt scan` runs, Then the process exits 1.
4. AC4 scan_exit_match_filter: Given one `not-installed` artifact named `foo` and the rest current, When `adopt scan --match 'bar*'` runs, Then the process exits 0.
5. AC5 docket_bridge_resolve_argv: Given a stub `docket` binary on PATH that records its argv, When `adopt report` resolves a finding with slug `wm-node`, Then the stub receives exactly `resolve wm-node` (plus `--reason <text>` if and only if a reason is passed) and no `--run` or `--key` token.
6. AC6 docket_bridge_error_log: Given a stub `docket` that exits 2 on any argv, When `adopt report` runs, Then stderr contains the full argv adopt attempted before the exit-status line.
7. AC7 docket_bridge_live: Given the real `docket` binary installed at ~/.local/bin/docket, When `adopt report --run <fresh-run-id>` runs on this box, Then it exits 0 with no clap usage error in output.
