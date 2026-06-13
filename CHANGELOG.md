# Changelog

## v0.5.0 — 2026-06-13

Classify stale artifacts into 6 named buckets; adopt verify reports them; docket gets per-reason slugs.

## v0.4.0 — 2026-06-13

Adds a validate_root guard that rejects any cargo install --root containing a literal tilde or resolving outside $HOME (the bug that created /home/jsy/~/.local/bin/ac-judge), plus a new adopt doctor [--clean] subcommand that scans for and optionally removes adopt-created debris under literal-tilde junk prefixes, removing only binaries that have a correctly-installed twin in the real ~/.local/bin or ~/.cargo/bin and never blindly deleting twin-less entries.

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
