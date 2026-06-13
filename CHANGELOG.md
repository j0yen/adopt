# Changelog

## v0.7.0 — 2026-06-13

Lineage-based freshness verdict (scion-verdict): `adopt scan` now consults the `InstallMarker` fingerprint written by `adopt apply` instead of comparing timestamps. A binary is `installed-current` iff its marker fingerprint equals the repo's current committed-HEAD hash, eliminating the chronic false-positive where binaries installed 5–33 seconds before their commit were always reported stale. Clock comparison is retained as a fallback when no marker exists. A new `freshness_basis` field on every artifact record (`"lineage"` or `"clock-fallback"`) lets callers distinguish proven from heuristic verdicts.

## v0.6.0 — 2026-06-13

Incremental install skip via source fingerprint — computes SHA-256 of source path and skips reinstall when fingerprint matches the installed binary's recorded hash, avoiding redundant reinstalls.

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
