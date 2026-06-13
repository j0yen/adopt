# Changelog

## v0.3.0 — adopt-docket-report

`adopt report`: wire unadopted artifacts to the docket ledger.
Writes a severity-tagged finding per not-installed artifact to the docket,
skipping artifacts already present, with idempotent upsert semantics.

## v0.2.0 — adopt-apply

`adopt apply`: install non-daemon unadopted artifacts, one at a time.
Default is dry-run; `--execute` performs installs via the artifact's
`fix_cmd`. Daemon artifacts are skipped and delegated to `rollout install`.
Stops on first failure (no cascade). Injection-safe: no artifact-derived
string reaches a shell.
